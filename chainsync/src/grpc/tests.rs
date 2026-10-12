use super::{
    matcher::Matcher,
    session::{CANDIDATES_FILTER, CONFINED_FILTER, RECORDS_FILTER, Session, TrackedAccount},
    transaction::undelegated_accounts,
    *,
};
use crate::transport::{Account, DELEGATION_SLOT, UNAVAILABLE, within, yellowstone::Server};
use dlp_api::{
    discriminator::DlpDiscriminator,
    pda::{
        delegation_record_pda_from_delegated_account,
        undelegation_request_pda_from_delegated_account,
    },
    state::{DelegationRecord, UndelegationRequest},
};
use engine::{Engine, testkit::TestEngine};
use futures::StreamExt;
use solana_account::AccountMode;
use solana_sdk_ids::sysvar::clock;
use tokio::{
    sync::mpsc,
    time::{self, Instant},
};
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::{
    geyser::subscribe_update::UpdateOneof,
    prelude::{
        CompiledInstruction, InnerInstruction, InnerInstructions, Message, SubscribeUpdateAccount,
        SubscribeUpdateAccountInfo, SubscribeUpdateTransactionInfo, Transaction,
        TransactionStatusMeta,
    },
};

const DELEGATION_LAMPORTS: u64 = 100;
const TRACKING_DELAY: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_nanos(1);
const EVENT_CAPACITY: usize = 8;
const CLOCK_SLOT: u64 = 80;

/// Metadata observed in the current slot, independent of the application image's wire contents.
fn delegation_record(authority: Pubkey, slot: u64) -> DelegationRecord {
    DelegationRecord {
        authority,
        owner: Pubkey::new_unique(),
        delegation_slot: slot,
        lamports: DELEGATION_LAMPORTS,
        commit_frequency_ms: 1,
    }
}

/// Distinct application contents make latest-observation selection observable.
fn account_info(data: u8) -> SubscribeUpdateAccountInfo {
    SubscribeUpdateAccountInfo {
        lamports: DELEGATION_LAMPORTS,
        data: vec![data],
        ..Default::default()
    }
}

/// Policy-only session with a ten-second delay; transport tests use the real plugin separately.
fn session(engine: Engine, events: mpsc::Sender<Event>) -> Session {
    Session::new(
        0,
        StreamConfig {
            endpoint: UNAVAILABLE.parse().unwrap(),
            token: None,
            duplication_delay: TRACKING_DELAY,
        },
        engine,
        events,
    )
    .unwrap()
}

/// Both arrival orders use the latest same-side observation, restore ownership, and select confinement.
#[test]
fn delegation_matches_latest_observations_in_both_orders_and_restores_ownership() {
    for confined in [false, true] {
        for account_first in [false, true] {
            let authority = Pubkey::new_unique();
            let key = Pubkey::new_unique();
            let pda = delegation_record_pda_from_delegated_account(&key);
            let metadata = delegation_record(
                if confined { Pubkey::default() } else { authority },
                DELEGATION_SLOT,
            );
            let mut matcher = Matcher::new(authority);
            matcher.set_slot(DELEGATION_SLOT);
            let result = if account_first {
                // Repeated observations replace only their own side of the pair.
                matcher.account(key, account_info(0));
                assert!(matcher.account(key, account_info(1)).is_none());
                matcher.record(pda, &account_info(2), &metadata)
            } else {
                matcher.record(
                    pda,
                    &account_info(0),
                    &delegation_record(metadata.authority, DELEGATION_SLOT),
                );
                assert!(matcher.record(pda, &account_info(2), &metadata).is_none());
                matcher.account(key, account_info(1))
            }
            .unwrap();
            assert_eq!(result.pubkey, key);
            assert_eq!(result.source_program, metadata.owner);
            assert_eq!(result.record, vec![2]);
            let account = result.account.read();
            assert_eq!(account.owner(), metadata.owner);
            assert_eq!(account.slot(), DELEGATION_SLOT);
            assert_eq!(account.data(), &[1]);
            assert_eq!(
                account.mode(),
                if confined { AccountMode::Magic } else { AccountMode::Delegated }
            );
            assert_eq!(
                account.lamports(),
                if confined { 0 } else { DELEGATION_LAMPORTS }
            );
        }
    }
}

/// Forward slot changes and backward replay discard incomplete matches in either arrival order.
#[test]
fn delegation_matching_discards_cross_slot_pairs_including_backward_replay() {
    let authority = Pubkey::new_unique();
    let key = Pubkey::new_unique();
    let pda = delegation_record_pda_from_delegated_account(&key);
    for (from, to) in
        [(DELEGATION_SLOT, DELEGATION_SLOT + 1), (DELEGATION_SLOT + 1, DELEGATION_SLOT)]
    {
        for account_first in [false, true] {
            let mut matcher = Matcher::new(authority);
            matcher.set_slot(from);
            if account_first {
                matcher.account(key, account_info(1));
            } else {
                matcher.record(pda, &account_info(2), &delegation_record(authority, from));
            }
            matcher.set_slot(to);
            let result = if account_first {
                matcher.record(pda, &account_info(2), &delegation_record(authority, to))
            } else {
                matcher.account(key, account_info(1))
            };
            assert!(result.is_none());
            let recovered = if account_first {
                matcher.account(key, account_info(3))
            } else {
                matcher.record(pda, &account_info(2), &delegation_record(authority, to))
            }
            .unwrap();
            assert_eq!(recovered.account.read().slot(), to);
        }
    }
}

/// Wrong authority and wrong delegation slot block matching for the remainder of the observed slot.
#[test]
fn invalid_delegation_authority_or_slot_blocks_matching_until_slot_changes() {
    let authority = Pubkey::new_unique();
    let key = Pubkey::new_unique();
    let pda = delegation_record_pda_from_delegated_account(&key);
    let valid_record = delegation_record(authority, DELEGATION_SLOT);
    for rejected in [
        delegation_record(Pubkey::new_unique(), DELEGATION_SLOT),
        delegation_record(authority, DELEGATION_SLOT - 1),
    ] {
        for account_first in [false, true] {
            let mut matcher = Matcher::new(authority);
            matcher.set_slot(DELEGATION_SLOT);
            if account_first {
                matcher.account(key, account_info(1));
            }
            assert!(matcher.record(pda, &account_info(2), &rejected).is_none());
            assert!(matcher.account(key, account_info(1)).is_none());
            // A valid record in the same slot must not undo the rejection.
            assert!(matcher.record(pda, &account_info(2), &valid_record).is_none());
            matcher.set_slot(DELEGATION_SLOT + 1);
            let next_record = delegation_record(authority, DELEGATION_SLOT + 1);
            assert!(matcher.record(pda, &account_info(2), &next_record).is_none());
            assert!(matcher.account(key, account_info(1)).is_some());
        }
    }
}

/// Eligibility is inclusive at the delay boundary; removals take effect on the next filter rebuild.
#[tokio::test]
async fn retained_filter_adds_at_delay_boundary_and_removes_on_rebuild() {
    let engine = TestEngine::new().await;
    let (events, _rx) = mpsc::channel(EVENT_CAPACITY);
    let mut session = session(engine.clone(), events);
    time::pause();
    let key = Pubkey::new_unique();
    session.desired.insert(
        key,
        TrackedAccount {
            sub: AccountSubscription { pubkey: key, program: None },
            generation: 1,
            tracked_at: Instant::now(),
        },
    );
    time::advance(TRACKING_DELAY - TICK).await;
    assert!(session.sync_filter().unwrap().is_none());
    assert!(!session.retained_filter.contains(key));
    time::advance(TICK).await;
    // Test eligibility by rebuilding explicitly; the service's periodic rebuild
    // may insert an eligible account later, but must never insert it earlier.
    assert_eq!(session.sync_filter().unwrap(), Some(vec![(key, 1)]));
    session.desired.remove(&key);
    assert!(
        session.retained_filter.contains(key),
        "removal from remote filter waits for rebuild"
    );
    assert_eq!(session.sync_filter().unwrap(), Some(vec![]));
    assert!(!session.retained_filter.contains(key));
    time::resume();
    drop(session);
    engine.close().await;
}

/// Only Clock advances the watermark; only a nonzero canonical DLP request emits an undelegation event.
#[tokio::test]
async fn only_clock_advances_watermark_and_only_canonical_requests_emit_events() {
    let engine = TestEngine::new().await;
    let (events, mut rx) = mpsc::channel(EVENT_CAPACITY);
    let mut session = session(engine.clone(), events);
    for (key, owner, slot) in [
        (Pubkey::new_unique(), Pubkey::default(), 100),
        (Pubkey::new_unique(), dlp_api::id(), 101),
        (clock::ID, Pubkey::default(), CLOCK_SLOT),
        (clock::ID, Pubkey::default(), 70),
    ] {
        session
            .account(SubscribeUpdateAccount {
                account: Some(SubscribeUpdateAccountInfo {
                    pubkey: key.to_bytes().to_vec(),
                    owner: owner.to_bytes().to_vec(),
                    lamports: 1,
                    ..Default::default()
                }),
                slot,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            engine.accounts().chain_slot(),
            if key == clock::ID { CLOCK_SLOT } else { 0 }
        );
    }

    let key = Pubkey::new_unique();
    let pda = undelegation_request_pda_from_delegated_account(&key);
    let request = UndelegationRequest {
        delegated_account: key,
        expires_at_slot: 50,
    };
    let mut data = vec![0; UndelegationRequest::size_with_discriminator()];
    request.to_bytes_with_discriminator(&mut data).unwrap();
    for (address, owner, lamports, requested) in [
        (Pubkey::new_unique(), dlp_api::id(), 1, false),
        (pda, Pubkey::default(), 1, false),
        (pda, dlp_api::id(), 0, false),
        (pda, dlp_api::id(), 1, true),
    ] {
        session
            .account(SubscribeUpdateAccount {
                account: Some(SubscribeUpdateAccountInfo {
                    pubkey: address.to_bytes().to_vec(),
                    owner: owner.to_bytes().to_vec(),
                    lamports,
                    data: data.clone(),
                    ..Default::default()
                }),
                slot: DELEGATION_SLOT,
                ..Default::default()
            })
            .await
            .unwrap();
        if requested {
            assert!(
                matches!(rx.try_recv(), Ok(Event::UndelegationRequested { pubkey, slot: DELEGATION_SLOT }) if pubkey == key)
            );
        } else {
            assert!(rx.try_recv().is_err());
        }
    }
    drop(session);
    engine.close().await;
}

/// The actual plugin applies ChainSync's confirmed Clock, cuckoo, DLP owner, and authority filters to replay.
#[tokio::test]
async fn plugin_applies_account_and_dlp_filters_and_preserves_clock_after_rebuild() {
    let engine = TestEngine::new().await;
    let server = Server::new().await;
    let (events, _rx) = mpsc::channel(EVENT_CAPACITY);
    let mut session = session(engine.clone(), events);
    let mut clock_account = Account::new(clock::ID);
    clock_account.data = vec![];
    let retained = Account::new(Pubkey::new_unique());
    let unrelated = Account::new(Pubkey::new_unique());
    let metadata = DelegationRecord {
        authority: session.authority,
        owner: Pubkey::new_unique(),
        delegation_slot: DELEGATION_SLOT,
        lamports: DELEGATION_LAMPORTS,
        commit_frequency_ms: 1,
    };
    let mut record = Account::new(Pubkey::new_unique());
    record.owner = dlp_api::id();
    record.data.resize(DelegationRecord::size_with_discriminator(), 0);
    metadata.to_bytes_with_discriminator(&mut record.data).unwrap();
    let mut wrong_owner = record.clone();
    wrong_owner.key = Pubkey::new_unique();
    wrong_owner.owner = Pubkey::new_unique();
    let mut wrong_authority = record.clone();
    wrong_authority.key = Pubkey::new_unique();
    DelegationRecord {
        authority: Pubkey::new_unique(),
        ..metadata
    }
    .to_bytes_with_discriminator(&mut wrong_authority.data)
    .unwrap();
    let mut confined = record.clone();
    confined.key = Pubkey::new_unique();
    DelegationRecord {
        authority: Pubkey::default(),
        ..metadata
    }
    .to_bytes_with_discriminator(&mut confined.data)
    .unwrap();
    server
        .seed(
            DELEGATION_SLOT,
            &[
                clock_account,
                retained.clone(),
                unrelated,
                record.clone(),
                wrong_owner,
                wrong_authority.clone(),
                confined.clone(),
            ],
        )
        .await;
    session.retained_filter.insert(retained.key).unwrap();
    let mut client = GeyserGrpcClient::build_from_shared(server.endpoint.to_string())
        .unwrap()
        .connect()
        .await
        .unwrap();
    let (mut sink, mut stream) = client
        .subscribe_with_request(Some(session.request(Some(DELEGATION_SLOT))))
        .await
        .unwrap();
    let mut received = Vec::new();
    let mut expected = vec![clock::ID, retained.key, record.key, wrong_authority.key, confined.key];
    within(async {
        while received.len() < expected.len() {
            let update = stream.next().await.unwrap().unwrap();
            if let Some(UpdateOneof::Account(account)) = update.update_oneof {
                let key = pubkey(&account.account.unwrap().pubkey).unwrap();
                if key == record.key {
                    assert!(update.filters.iter().any(|label| label == RECORDS_FILTER));
                } else if key == wrong_authority.key {
                    // Other validators' records remain candidates but must not match an authority filter.
                    assert_eq!(update.filters, vec![CANDIDATES_FILTER]);
                } else if key == confined.key {
                    assert!(update.filters.iter().any(|label| label == CONFINED_FILTER));
                    assert!(!update.filters.iter().any(|label| label == RECORDS_FILTER));
                }
                received.push(key);
            }
        }
    })
    .await;
    received.sort_unstable();
    expected.sort_unstable();
    assert_eq!(received, expected);
    // Rebuilding an empty desired set drops the cuckoo filter but retains mandatory Clock coverage.
    assert_eq!(session.sync_filter().unwrap(), Some(vec![]));
    session.refresh(&mut sink).await.unwrap();
    server.confirmed(DELEGATION_SLOT + 1, &[Account::new(clock::ID)]);
    within(async {
        loop {
            let update = stream.next().await.unwrap().unwrap();
            if let Some(UpdateOneof::Account(account)) = update.update_oneof {
                assert_eq!(account.slot, DELEGATION_SLOT + 1);
                assert_eq!(
                    account.account.as_ref().unwrap().pubkey,
                    clock::ID.to_bytes()
                );
                session.account(account).await.unwrap();
                break;
            }
        }
    })
    .await;
    assert_eq!(engine.accounts().chain_slot(), DELEGATION_SLOT + 1);
    drop(sink);
    drop(stream);
    drop(client);
    drop(session);
    server.close().await;
    engine.close().await;
}

/// Outer/CPI releases require local authority, except timeout rollback; targets deduplicate
/// and malformed CPI cannot return partial results.
#[test]
fn undelegation_targets_deduplicate_outer_and_cpi_without_partial_decode() {
    let target = Pubkey::new_unique();
    let authority = Pubkey::new_unique();
    let outer = CompiledInstruction {
        program_id_index: 0,
        accounts: vec![2, 1],
        data: DlpDiscriminator::Undelegate.to_vec(),
    };
    // The CPI repeats the same release so decoding must collapse it to one target.
    let inner = InnerInstruction {
        program_id_index: outer.program_id_index,
        accounts: outer.accounts.clone(),
        data: outer.data.clone(),
        ..Default::default()
    };
    let mut tx = SubscribeUpdateTransactionInfo {
        transaction: Some(Transaction {
            message: Some(Message {
                account_keys: vec![
                    dlp_api::id().to_bytes().to_vec(),
                    target.to_bytes().to_vec(),
                    authority.to_bytes().to_vec(),
                ],
                instructions: vec![
                    outer,
                    CompiledInstruction {
                        program_id_index: 2,
                        accounts: vec![],
                        data: DlpDiscriminator::Undelegate.to_vec(),
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        }),
        meta: Some(TransactionStatusMeta {
            inner_instructions: vec![InnerInstructions {
                index: 0,
                instructions: vec![inner],
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        undelegated_accounts(&tx, authority).unwrap().as_slice(),
        &[target]
    );
    let foreign = Pubkey::new_unique();
    assert!(undelegated_accounts(&tx, foreign).unwrap().is_empty());

    // A timeout rollback has no validator account and must survive the authority filter.
    let inner = &mut tx.meta.as_mut().unwrap().inner_instructions[0].instructions[0];
    inner.accounts = vec![1];
    inner.data = DlpDiscriminator::UndelegateWithRollbackAfterTimeout.to_vec();
    assert_eq!(
        undelegated_accounts(&tx, foreign).unwrap().as_slice(),
        &[target]
    );
    // A valid outer release must not escape when a later CPI cannot be decoded.
    for (accounts, error) in
        [(vec![], "missing undelegated account"), (vec![3], "invalid account index")]
    {
        tx.meta.as_mut().unwrap().inner_instructions[0].instructions[0].accounts = accounts;
        assert!(
            matches!(undelegated_accounts(&tx, authority), Err(Error::Protocol(cause)) if cause == error)
        );
    }
}
