use dlp_api::{pda::delegation_record_pda_from_delegated_account, state::DelegationRecord};
use engine::testkit::TestEngine;
use hyper::StatusCode;
use keeper::testkit::{V42_ID, store_v42, v42_builder};
use solana_account::{AccountMode, AccountSharedData, ReadableAccount};
use solana_program_pack::Pack;
use spl_token::state::Account as TokenAccount;

use super::{
    coverage::{Coverage, Source},
    *,
};
use crate::delegation::{self, Delegation};
use crate::transport::magic::intents;
use crate::transport::{
    Account, Ata, CONTEXT_SLOT, DELEGATED_TOKEN_BALANCE, DELEGATION_SLOT, FIRST_SUBSCRIPTION,
    PROJECTION_SLOT, SERVICE_ERROR, TOKEN_BALANCE, V42_INITIAL, delegation as delegation_images,
    encode_record, http::Server, magic, scripted, start_chain_sync, start_handler_sync, websocket,
    within, yellowstone,
};
use crate::websocket::Event;
use crate::{AccountSubscription, Error, HttpError, WebSocketError, ata};

const LOCAL_BALANCE: i64 = 9;
const NEWER_SLOT: u64 = 20;
const BUFFERED_SLOT: u64 = 100;

/// Ordinary, confined, and projected gRPC delegations retire tracking and reject buffered updates and confirmations.
#[tokio::test]
async fn grpc_delegation_retires_tracking_and_rejects_buffered_mirror_updates() {
    let mut engine = TestEngine::new().await;
    let chain_sync = start_handler_sync(&mut engine, None);
    let mut cases = Vec::new();
    for mode in [AccountMode::Delegated, AccountMode::Magic] {
        let key = Pubkey::new_unique();
        engine
            .accounts()
            .store(&[(
                key,
                v42_builder(LOCAL_BALANCE, AccountMode::Uninit).slot(CONTEXT_SLOT).build(),
            )])
            .unwrap();
        let authority =
            if mode == AccountMode::Magic { Pubkey::default() } else { engine.authority() };
        let (_, record) = delegation_images(key, V42_ID, authority, DELEGATION_SLOT, &[]);
        let mut account = v42_builder(V42_INITIAL, mode).slot(DELEGATION_SLOT);
        if mode == AccountMode::Magic {
            account = account.lamports(0);
        }
        cases.push((
            key,
            Delegation {
                pubkey: key,
                account,
                source_program: V42_ID,
                record: record.data,
            },
        ));
    }
    let Ata {
        ata: key,
        base,
        eata,
        delegated: account,
        record,
    } = Ata::new(spl_token::id(), false, engine.authority(), PROJECTION_SLOT);
    let lamports = engine.rent().minimum_balance(base.read().data().len());
    let base = base.lamports(lamports);
    engine.accounts().store(&[(key, base.build())]).unwrap();
    cases.push((
        key,
        Delegation {
            pubkey: eata,
            source_program: account.read().owner(),
            account,
            record,
        },
    ));
    for (key, delegation) in cases {
        let mode = delegation.account.read().mode();
        let slot = delegation.account.read().slot();
        let sub = AccountSubscription { pubkey: key, program: None };
        let mut coverage = Coverage::default();
        let generation = coverage.acknowledged(sub);
        coverage.confirmed(0, key, generation);
        if mode == AccountMode::Magic {
            // Delegation must also retire tracking when only its gRPC copy remains.
            coverage.lost(Source::WebSocket, key);
        }
        within(chain_sync.on_grpc(grpc::Event::Delegated(delegation), &mut coverage))
            .await
            .unwrap();
        let accepted = engine.get_account(key).unwrap();
        assert_eq!(accepted.mode(), mode);
        assert_eq!(accepted.slot(), slot);
        // An old filter acknowledgement cannot re-enable coverage after delegation.
        coverage.confirmed(0, key, generation);
        assert!(!coverage.ws_contains(sub));
        assert!(!coverage.grpc_contains(0, sub));
        chain_sync
            .on_websocket(
                Event::Update {
                    sub,
                    account: v42_builder(LOCAL_BALANCE, AccountMode::Uninit).slot(BUFFERED_SLOT),
                },
                &mut coverage,
            )
            .await
            .unwrap();
        chain_sync
            .on_grpc(
                grpc::Event::Update {
                    stream: 0,
                    sub,
                    account: v42_builder(LOCAL_BALANCE, AccountMode::Uninit)
                        .slot(BUFFERED_SLOT + 1),
                },
                &mut coverage,
            )
            .await
            .unwrap();
        assert_eq!(engine.get_account(key).unwrap(), accepted);
    }
    assert_eq!(
        engine.get_account(eata).unwrap().mode(),
        AccountMode::ReadOnly
    );
    drop(chain_sync);
    engine.close().await;
}

/// Requests transition once; releases refresh eligible state, preserve newer/authoritative images, and delete on HTTP exhaustion.
#[tokio::test]
async fn undelegation_transitions_once_and_confirmations_preserve_newer_or_authoritative_state() {
    let mut engine = magic::engine().await;
    let mut http = Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let chain_sync = start_chain_sync(&mut engine, &http, Some(&ws), &grpc);
    ws.wait_for_connection().await;
    let mut coverage = Coverage::default();
    let key = Pubkey::new_unique();
    engine
        .accounts()
        .store(&[(
            key,
            v42_builder(V42_INITIAL, AccountMode::Delegated).slot(DELEGATION_SLOT).build(),
        )])
        .unwrap();
    for (slot, mode, count) in [
        (DELEGATION_SLOT - 1, AccountMode::Delegated, 0),
        (DELEGATION_SLOT, AccountMode::Transient, 1),
        (DELEGATION_SLOT + 1, AccountMode::Transient, 1),
    ] {
        within(chain_sync.undelegation_requested(key, slot)).await.unwrap();
        assert_eq!(engine.get_account(key).unwrap().mode(), mode);
        assert_eq!(intents(&engine), count);
    }
    let readonly = store_v42(&engine, V42_INITIAL, AccountMode::ReadOnly);
    within(chain_sync.undelegation_requested(readonly, BUFFERED_SLOT))
        .await
        .unwrap();
    assert_eq!(intents(&engine), 1);
    within(chain_sync.undelegated(key, DELEGATION_SLOT - 1, &mut coverage))
        .await
        .unwrap();
    assert_eq!(
        engine.get_account(key).unwrap().mode(),
        AccountMode::Transient
    );
    let image = Account::from_builder(key, &v42_builder(LOCAL_BALANCE, AccountMode::ReadOnly));
    let script = async {
        let call = ws.next().await;
        assert_eq!(call.pubkey, key);
        call.ack(FIRST_SUBSCRIPTION);
        let call = http.next().await;
        assert_eq!(call.min_context_slot(), Some(DELEGATION_SLOT));
        // Neither subscription acknowledgement nor an in-flight HTTP request demotes local state.
        assert_eq!(
            engine.get_account(key).unwrap().mode(),
            AccountMode::Transient
        );
        call.respond_accounts(DELEGATION_SLOT, &[Some(image)]);
    };
    scripted(
        chain_sync.undelegated(key, DELEGATION_SLOT, &mut coverage),
        script,
    )
    .await
    .unwrap();
    let refreshed = engine.get_account(key).unwrap();
    assert_eq!(refreshed.mode(), AccountMode::ReadOnly);
    assert_eq!(refreshed.data(), &LOCAL_BALANCE.to_le_bytes());
    for (mode, observed) in [
        (AccountMode::Uninit, DELEGATION_SLOT + 1),
        (AccountMode::ReadOnly, DELEGATION_SLOT + 1),
        (AccountMode::Delegated, DELEGATION_SLOT),
        (AccountMode::Magic, DELEGATION_SLOT),
        (AccountMode::System, DELEGATION_SLOT + 1),
    ] {
        let key = Pubkey::new_unique();
        engine
            .accounts()
            .store(&[(key, v42_builder(V42_INITIAL, mode).slot(observed).build())])
            .unwrap();
        let before = engine.get_account(key).unwrap();
        within(chain_sync.undelegated(key, DELEGATION_SLOT, &mut coverage))
            .await
            .unwrap();
        assert_eq!(engine.get_account(key).unwrap(), before);
    }
    assert!(!http.pending());

    let absent = store_v42(&engine, V42_INITIAL, AccountMode::Transient);
    let script = async {
        ws.next().await.ack(FIRST_SUBSCRIPTION + 1);
        http.next().await.respond_accounts(DELEGATION_SLOT, &[None]);
    };
    scripted(
        chain_sync.undelegated(absent, DELEGATION_SLOT, &mut coverage),
        script,
    )
    .await
    .unwrap();
    let account = engine.get_account(absent).unwrap();
    assert_eq!(account.mode(), AccountMode::Uninit);
    assert_eq!(account.slot(), DELEGATION_SLOT);

    // A terminal provider response must not delete state installed while HTTP was in flight.
    let protected = store_v42(&engine, V42_INITIAL, AccountMode::Transient);
    let script = async {
        ws.next().await.ack(FIRST_SUBSCRIPTION + 2);
        let call = http.next().await;
        engine
            .accounts()
            .store(&[(
                protected,
                v42_builder(LOCAL_BALANCE, AccountMode::Magic).slot(DELEGATION_SLOT + 1).build(),
            )])
            .unwrap();
        call.respond(StatusCode::OK, "{}");
    };
    let result = scripted(
        chain_sync.undelegated(protected, DELEGATION_SLOT, &mut coverage),
        script,
    )
    .await;
    assert!(matches!(result, Err(Error::Fetch(_))));
    assert_eq!(
        engine.get_account(protected).unwrap().mode(),
        AccountMode::Magic
    );

    let failed = store_v42(&engine, V42_INITIAL, AccountMode::Transient);
    let mut attempts = 0;
    let result = {
        let script = async {
            ws.next().await.ack(FIRST_SUBSCRIPTION + 3);
            loop {
                let call = http.next().await;
                attempts += 1;
                assert_eq!(
                    engine.get_account(failed).unwrap().mode(),
                    AccountMode::Transient
                );
                call.respond(StatusCode::SERVICE_UNAVAILABLE, SERVICE_ERROR);
            }
        };
        tokio::pin!(script);
        within(async {
            tokio::select! {
                result = chain_sync.undelegated(failed, DELEGATION_SLOT, &mut coverage) => result,
                _ = &mut script => unreachable!("provider responds until the retry budget expires"),
            }
        })
        .await
    };
    assert!(matches!(
        result,
        Err(Error::Fetch(HttpError::Deadline { .. }))
    ));
    assert!(attempts > 1, "deletion waits for the HTTP retry budget");
    assert!(engine.get_account(failed).is_none());
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}

/// Obsolete updates cannot resurrect coverage; explicit release preserves a reacquired mirror and rejects buffered updates.
#[tokio::test]
async fn stale_confirmations_wrong_streams_and_removed_subscriptions_reject_updates() {
    let mut engine = TestEngine::new().await;
    let chain_sync = start_handler_sync(&mut engine, None);
    let key = Pubkey::new_unique();
    let sub = AccountSubscription { pubkey: key, program: None };
    let mut coverage = Coverage::default();
    let old_generation = coverage.acknowledged(sub);
    coverage.remove(sub);
    let current_generation = coverage.acknowledged(sub);
    let mirror_update = |stream| grpc::Event::Update {
        stream,
        sub,
        account: v42_builder(V42_INITIAL, AccountMode::Uninit).slot(DELEGATION_SLOT),
    };
    // A delayed confirmation for the previous acquisition cannot grant coverage.
    coverage.confirmed(0, key, old_generation);
    chain_sync.on_grpc(mirror_update(0), &mut coverage).await.unwrap();
    assert!(engine.get_account(key).is_none());
    coverage.confirmed(0, key, current_generation);
    chain_sync.on_grpc(mirror_update(1), &mut coverage).await.unwrap();
    assert!(engine.get_account(key).is_none());
    coverage.remove(sub);
    chain_sync.on_grpc(mirror_update(0), &mut coverage).await.unwrap();
    assert!(engine.get_account(key).is_none());
    coverage.acknowledged(sub);
    engine
        .accounts()
        .store(&[(
            key,
            v42_builder(LOCAL_BALANCE, AccountMode::Uninit).slot(NEWER_SLOT).build(),
        )])
        .unwrap();
    // Removal revokes coverage only. Deletion belongs to the eviction caller,
    // so delayed transport cleanup must not delete a newly installed account.
    chain_sync.on_websocket(Event::Removed(key), &mut coverage).await.unwrap();
    chain_sync
        .on_websocket(
            Event::Update {
                sub,
                account: v42_builder(V42_INITIAL, AccountMode::Uninit).slot(NEWER_SLOT + 1),
            },
            &mut coverage,
        )
        .await
        .unwrap();
    let account = engine
        .get_account(key)
        .expect("old cleanup must preserve the reacquired mirror");
    assert_eq!(account.data(), LOCAL_BALANCE.to_le_bytes());
    assert_eq!(account.slot(), NEWER_SLOT);
    drop(coverage);
    drop(chain_sync);
    engine.close().await;
}

/// A completion that discovers redelegation retains raw readonly and activates only the newer projection.
#[tokio::test]
async fn raw_eata_undelegation_refreshes_only_eligible_ata_projection() {
    let mut engine = TestEngine::new().await;
    let mut http = Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let chain_sync = start_chain_sync(&mut engine, &http, Some(&ws), &grpc);
    ws.wait_for_connection().await;
    let mut coverage = Coverage::default();
    let authority = engine.authority();
    let Ata {
        ata: key,
        base,
        eata,
        delegated,
        record,
    } = Ata::new(spl_token::id(), false, authority, PROJECTION_SLOT);
    let projected = ata::project(key, base.clone(), eata, &delegated, &record, authority)
        .unwrap()
        .mode(AccountMode::Transient);
    engine
        .accounts()
        .store(&[(key, projected.build()), (eata, ata::normalize_raw(delegated.clone()).build())])
        .unwrap();
    let delegated = delegated.slot(PROJECTION_SLOT + 1);
    let metadata = DelegationRecord {
        delegation_slot: PROJECTION_SLOT + 1,
        ..*delegation::record(&record).unwrap()
    };
    let mut raw = Account::from_builder(eata, &delegated);
    raw.owner = dlp_api::id();
    let remote_record = Account {
        owner: dlp_api::id(),
        data: encode_record(&metadata),
        ..Account::new(delegation_record_pda_from_delegated_account(&eata))
    };
    let script = async {
        ws.next().await.ack(FIRST_SUBSCRIPTION);
        let call = http.next().await;
        assert_eq!(call.min_context_slot(), Some(PROJECTION_SLOT));
        call.respond_accounts(PROJECTION_SLOT + 1, &[Some(raw.clone())]);
        http.next().await.respond_accounts(
            PROJECTION_SLOT + 1,
            &[Some(raw.clone()), Some(remote_record.clone())],
        );
        let call = ws.next().await;
        assert_eq!(call.pubkey, key);
        call.ack(FIRST_SUBSCRIPTION + 1);
        http.next().await.respond_accounts(
            PROJECTION_SLOT + 1,
            &[Some(Account::from_builder(key, &base))],
        );
        http.next()
            .await
            .respond_accounts(PROJECTION_SLOT + 1, &[Some(raw), Some(remote_record)]);
    };
    let result = scripted(
        async {
            // Establish the completion's raw subscription before testing the
            // paired refresh, rather than retrying partially completed work.
            loop {
                match chain_sync
                    .websocket
                    .subscribe(AccountSubscription { pubkey: eata, program: None }, true)
                    .await
                {
                    Err(WebSocketError::Unavailable) => continue,
                    result => {
                        result?;
                        break;
                    }
                }
            }
            chain_sync.undelegated(eata, PROJECTION_SLOT, &mut coverage).await
        },
        script,
    )
    .await;
    result.unwrap();
    let account = engine.get_account(key).unwrap();
    assert_eq!(account.mode(), AccountMode::Delegated);
    assert_eq!(account.slot(), PROJECTION_SLOT + 1);
    assert_eq!(account.owner(), &spl_token::id());
    assert_eq!(
        TokenAccount::unpack(account.data()).unwrap().amount,
        DELEGATED_TOKEN_BALANCE
    );
    assert_eq!(ws.unsubscribed().await, FIRST_SUBSCRIPTION);
    assert_eq!(ws.unsubscribed().await, FIRST_SUBSCRIPTION + 1);
    assert_eq!(
        engine.get_account(eata).unwrap().mode(),
        AccountMode::ReadOnly
    );
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}

/// Raw-address requests schedule only the projection, and completion still refreshes it when raw closes.
#[tokio::test]
async fn raw_eata_requests_and_closure_preserve_the_projection_lifecycle() {
    let mut engine = magic::engine().await;
    let mut http = Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let chain_sync = start_chain_sync(&mut engine, &http, Some(&ws), &grpc);
    ws.wait_for_connection().await;
    let mut fixture = Ata::new(spl_token::id(), false, engine.authority(), PROJECTION_SLOT);
    fixture.base = fixture.base.lamports(engine.rent().minimum_balance(TokenAccount::LEN));
    let projected = ata::project(
        fixture.ata,
        fixture.base.clone(),
        fixture.eata,
        &fixture.delegated,
        &fixture.record,
        engine.authority(),
    )
    .unwrap();
    let raw: AccountSharedData = ata::normalize_raw(fixture.delegated.clone()).build();
    engine
        .accounts()
        .store(&[(fixture.ata, projected.build()), (fixture.eata, raw.clone())])
        .unwrap();

    within(chain_sync.undelegation_requested(fixture.eata, PROJECTION_SLOT - 1))
        .await
        .unwrap();
    assert_eq!(intents(&engine), 0);
    for _ in 0..2 {
        within(chain_sync.undelegation_requested(fixture.eata, PROJECTION_SLOT))
            .await
            .unwrap();
    }
    assert_eq!(intents(&engine), 1);
    assert_eq!(engine.get_account(fixture.eata).unwrap(), raw);
    assert_eq!(
        engine.get_account(fixture.ata).unwrap().mode(),
        AccountMode::Transient
    );

    let slot = PROJECTION_SLOT + 1;
    let script = async {
        let sub = ws.next().await;
        assert_eq!(sub.pubkey, fixture.eata);
        sub.ack(FIRST_SUBSCRIPTION);
        http.next().await.respond_accounts(slot, &[None]);
        let sub = ws.next().await;
        assert_eq!(sub.pubkey, fixture.ata);
        sub.ack(FIRST_SUBSCRIPTION + 1);
        http.next().await.respond_accounts(
            slot,
            &[Some(Account::from_builder(fixture.ata, &fixture.base))],
        );
        http.next().await.respond_accounts(slot, &[None, None]);
    };
    let mut coverage = Coverage::default();
    scripted(
        chain_sync.undelegated(fixture.eata, slot, &mut coverage),
        script,
    )
    .await
    .unwrap();
    assert_eq!(
        engine.get_account(fixture.eata).unwrap().mode(),
        AccountMode::Uninit
    );
    let restored = engine.get_account(fixture.ata).unwrap();
    assert_eq!(restored.mode(), AccountMode::ReadOnly);
    assert_eq!(
        TokenAccount::unpack(restored.data()).unwrap().amount,
        TOKEN_BALANCE
    );
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}

/// Last-source loss evicts mirrors but cannot delete delegated, confined, or already-transient accounts.
#[tokio::test]
async fn last_source_loss_evicts_only_non_authoritative_accounts() {
    let mut engine = TestEngine::new().await;
    let chain_sync = start_handler_sync(&mut engine, None);
    for (mode, preserved) in [
        (AccountMode::Uninit, false),
        (AccountMode::ReadOnly, false),
        (AccountMode::Delegated, true),
        (AccountMode::Magic, true),
        (AccountMode::Transient, true),
    ] {
        let key = Pubkey::new_unique();
        engine
            .accounts()
            .store(&[(
                key,
                v42_builder(LOCAL_BALANCE, mode).slot(DELEGATION_SLOT).build(),
            )])
            .unwrap();
        let mut coverage = Coverage::default();
        let sub = AccountSubscription { pubkey: key, program: None };
        let generation = coverage.acknowledged(sub);
        coverage.confirmed(0, key, generation);
        within(chain_sync.lost(&mut coverage, Source::WebSocket, key)).await.unwrap();
        assert!(engine.get_account(key).is_some());
        within(chain_sync.lost(&mut coverage, Source::Grpc, key)).await.unwrap();
        assert_eq!(engine.get_account(key).is_some(), preserved);
    }
    drop(chain_sync);
    engine.close().await;
}

/// Source loss preserves remaining coverage; last loss removes it and stale generations cannot restore it.
#[test]
fn coverage_survives_partial_loss_and_rejects_obsolete_generations() {
    for source in [Source::WebSocket, Source::Grpc] {
        let mut coverage = Coverage::default();
        let sub = AccountSubscription {
            pubkey: Pubkey::new_unique(),
            program: None,
        };
        let generation = coverage.acknowledged(sub);
        coverage.confirmed(2, sub.pubkey, generation);
        assert!(coverage.ws_contains(sub));
        assert!(coverage.grpc_contains(2, sub));
        assert!(!coverage.grpc_contains(1, sub));
        let wrong = AccountSubscription {
            program: Some(Pubkey::new_unique()),
            ..sub
        };
        assert!(!coverage.ws_contains(wrong));
        assert!(!coverage.grpc_contains(2, wrong));
        assert_eq!(coverage.lost(source, sub.pubkey), (false, None));
        let remaining = match source {
            Source::WebSocket => Source::Grpc,
            Source::Grpc => Source::WebSocket,
        };
        assert_eq!(
            coverage.lost(remaining, sub.pubkey),
            (true, Some(sub.pubkey))
        );
        assert!(!coverage.ws_contains(sub));
        assert!(!coverage.grpc_contains(2, sub));
        coverage.confirmed(2, sub.pubkey, generation);
        assert!(!coverage.grpc_contains(2, sub));
        let current = coverage.acknowledged(sub);
        assert_ne!(generation, current);
        coverage.confirmed(0, sub.pubkey, generation);
        assert!(!coverage.grpc_contains(0, sub));
        coverage.confirmed(1, sub.pubkey, current);
        coverage.confirmed(0, sub.pubkey, generation);
        assert!(!coverage.grpc_contains(0, sub));
        assert!(coverage.grpc_contains(1, sub));
        assert!(coverage.remove(sub));
        coverage.confirmed(1, sub.pubkey, current);
        assert!(!coverage.grpc_contains(1, sub));
    }
}

/// Program and ProgramData feed the same local target, which is evicted only after both disappear.
#[test]
fn program_and_program_data_loss_evict_the_same_local_program() {
    let program = Pubkey::new_unique();
    let [direct, data] = AccountSubscription::for_account(program);
    for [first, last] in [[direct, data], [data, direct]] {
        let mut coverage = Coverage::default();
        coverage.acknowledged(first);
        coverage.acknowledged(last);
        assert_eq!(coverage.removed(first.pubkey), None);
        assert_eq!(coverage.removed(last.pubkey), Some(program));
    }
}
