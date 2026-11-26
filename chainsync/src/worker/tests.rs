use delegation_program_api::{
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
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
use crate::support::magic::intents;
use crate::support::{
    Account, Ata, CONTEXT_SLOT, DELEGATED_TOKEN_BALANCE, DELEGATION_SLOT, FIRST_SUBSCRIPTION,
    PROJECTION_SLOT, SERVICE_ERROR, TOKEN_BALANCE, V42_INITIAL, delegation as delegation_images,
    encode_record, http::Server, magic, scripted, start_chain_sync, start_handler_sync, websocket,
    within, yellowstone,
};
use crate::websocket::{Event, Retention};
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
                v42_builder(LOCAL_BALANCE, AccountMode::Missing).slot(CONTEXT_SLOT).build(),
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
        let mut worker = Worker::new(&chain_sync);
        let generation = worker.coverage.acknowledged(sub);
        worker.coverage.confirmed(0, key, generation);
        if mode == AccountMode::Magic {
            // Delegation must also retire tracking when only its gRPC copy remains.
            worker.coverage.lost(Source::WebSocket, key);
        }
        within(worker.handle_grpc(grpc::Event::Delegated(delegation))).await.unwrap();
        let accepted = engine.get_account(key).unwrap();
        assert_eq!(accepted.mode(), mode);
        assert_eq!(accepted.slot(), slot);
        // An old filter acknowledgement cannot re-enable coverage after delegation.
        worker.coverage.confirmed(0, key, generation);
        assert!(!worker.coverage.ws_contains(sub));
        assert!(!worker.coverage.grpc_contains(0, sub));
        worker
            .handle_websocket(Event::Update {
                sub,
                account: v42_builder(LOCAL_BALANCE, AccountMode::Missing).slot(BUFFERED_SLOT),
            })
            .await
            .unwrap();
        worker
            .handle_grpc(grpc::Event::Update {
                stream: 0,
                sub,
                account: v42_builder(LOCAL_BALANCE, AccountMode::Missing).slot(BUFFERED_SLOT + 1),
            })
            .await
            .unwrap();
        assert_eq!(engine.get_account(key).unwrap(), accepted);
    }
    assert_eq!(
        engine.get_account(eata).unwrap().mode(),
        AccountMode::Mirrored
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
    let mut worker = Worker::new(&chain_sync);
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
        (DELEGATION_SLOT, AccountMode::Undelegating, 1),
        (DELEGATION_SLOT + 1, AccountMode::Undelegating, 1),
    ] {
        within(worker.undelegation_requested(key, slot)).await.unwrap();
        assert_eq!(engine.get_account(key).unwrap().mode(), mode);
        assert_eq!(intents(&engine), count);
    }
    let readonly = store_v42(&engine, V42_INITIAL, AccountMode::Mirrored);
    within(worker.undelegation_requested(readonly, BUFFERED_SLOT)).await.unwrap();
    assert_eq!(intents(&engine), 1);
    within(worker.undelegated(key, DELEGATION_SLOT - 1)).await.unwrap();
    assert_eq!(
        engine.get_account(key).unwrap().mode(),
        AccountMode::Undelegating
    );
    let image = Account::from_builder(key, &v42_builder(LOCAL_BALANCE, AccountMode::Mirrored));
    let script = async {
        let call = ws.next().await;
        assert_eq!(call.pubkey, key);
        call.ack(FIRST_SUBSCRIPTION);
        let call = http.next().await;
        assert_eq!(call.min_context_slot(), Some(DELEGATION_SLOT));
        // Neither subscription acknowledgement nor an in-flight HTTP request demotes local state.
        assert_eq!(
            engine.get_account(key).unwrap().mode(),
            AccountMode::Undelegating
        );
        call.respond_accounts(DELEGATION_SLOT, &[Some(image)]);
    };
    scripted(worker.undelegated(key, DELEGATION_SLOT), script).await.unwrap();
    let refreshed = engine.get_account(key).unwrap();
    assert_eq!(refreshed.mode(), AccountMode::Mirrored);
    assert_eq!(refreshed.data(), &LOCAL_BALANCE.to_le_bytes());
    for (mode, observed) in [
        (AccountMode::Missing, DELEGATION_SLOT + 1),
        (AccountMode::Mirrored, DELEGATION_SLOT + 1),
        (AccountMode::Delegated, DELEGATION_SLOT),
        (AccountMode::Magic, DELEGATION_SLOT),
        (AccountMode::Runtime, DELEGATION_SLOT + 1),
    ] {
        let key = Pubkey::new_unique();
        engine
            .accounts()
            .store(&[(key, v42_builder(V42_INITIAL, mode).slot(observed).build())])
            .unwrap();
        let before = engine.get_account(key).unwrap();
        within(worker.undelegated(key, DELEGATION_SLOT)).await.unwrap();
        assert_eq!(engine.get_account(key).unwrap(), before);
    }
    assert!(!http.pending());

    let absent = store_v42(&engine, V42_INITIAL, AccountMode::Undelegating);
    let script = async {
        ws.next().await.ack(FIRST_SUBSCRIPTION + 1);
        http.next().await.respond_accounts(DELEGATION_SLOT, &[None]);
    };
    scripted(worker.undelegated(absent, DELEGATION_SLOT), script).await.unwrap();
    let account = engine.get_account(absent).unwrap();
    assert_eq!(account.mode(), AccountMode::Missing);
    assert_eq!(account.slot(), DELEGATION_SLOT);

    // A terminal provider response must not delete state installed while HTTP was in flight.
    let protected = store_v42(&engine, V42_INITIAL, AccountMode::Undelegating);
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
    let result = scripted(worker.undelegated(protected, DELEGATION_SLOT), script).await;
    assert!(matches!(result, Err(Error::Fetch(_))));
    assert_eq!(
        engine.get_account(protected).unwrap().mode(),
        AccountMode::Magic
    );

    let failed = store_v42(&engine, V42_INITIAL, AccountMode::Undelegating);
    let mut attempts = 0;
    let result = {
        let script = async {
            ws.next().await.ack(FIRST_SUBSCRIPTION + 3);
            loop {
                let call = http.next().await;
                attempts += 1;
                assert_eq!(
                    engine.get_account(failed).unwrap().mode(),
                    AccountMode::Undelegating
                );
                call.respond(StatusCode::SERVICE_UNAVAILABLE, SERVICE_ERROR);
            }
        };
        tokio::pin!(script);
        within(async {
            tokio::select! {
                result = worker.undelegated(failed, DELEGATION_SLOT) => result,
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

/// Subscription failure deletes eligible released images but preserves protected or newer state.
#[tokio::test]
async fn subscription_failure_deletes_only_eligible_undelegating_state() {
    let mut engine = TestEngine::new().await;
    let chain_sync = start_handler_sync(&mut engine, None);
    let mut worker = Worker::new(&chain_sync);
    for (mode, slot, delete) in [
        (AccountMode::Undelegating, DELEGATION_SLOT - 1, true),
        (AccountMode::Undelegating, NEWER_SLOT, false),
        (AccountMode::Delegated, DELEGATION_SLOT - 1, false),
        (AccountMode::Magic, DELEGATION_SLOT - 1, false),
    ] {
        let key = Pubkey::new_unique();
        let account: AccountSharedData = v42_builder(V42_INITIAL, mode).slot(slot).build();
        engine.accounts().store(&[(key, account.clone())]).unwrap();
        let result = worker.undelegated(key, DELEGATION_SLOT).await;
        if delete {
            // This fixture has no WebSocket providers, so subscription capacity is zero.
            assert!(matches!(
                result,
                Err(Error::Subscribe(WebSocketError::Capacity))
            ));
            assert!(engine.get_account(key).is_none());
        } else {
            result.unwrap();
            assert_eq!(engine.get_account(key).unwrap(), account);
        }
    }
    drop(chain_sync);
    engine.close().await;
}

/// Obsolete updates cannot resurrect coverage; explicit release preserves a reacquired mirror and rejects buffered updates.
#[tokio::test]
async fn stale_confirmations_wrong_streams_and_removed_subscriptions_reject_updates() {
    let mut engine = TestEngine::new().await;
    let chain_sync = start_handler_sync(&mut engine, None);
    let key = Pubkey::new_unique();
    let sub = AccountSubscription { pubkey: key, program: None };
    let mut worker = Worker::new(&chain_sync);
    let old_generation = worker.coverage.acknowledged(sub);
    worker.coverage.remove(sub);
    let current_generation = worker.coverage.acknowledged(sub);
    let mirror_update = |stream| grpc::Event::Update {
        stream,
        sub,
        account: v42_builder(V42_INITIAL, AccountMode::Missing).slot(DELEGATION_SLOT),
    };
    // A delayed confirmation for the previous acquisition cannot grant coverage.
    worker.coverage.confirmed(0, key, old_generation);
    worker.handle_grpc(mirror_update(0)).await.unwrap();
    assert!(engine.get_account(key).is_none());
    worker.coverage.confirmed(0, key, current_generation);
    worker.handle_grpc(mirror_update(1)).await.unwrap();
    assert!(engine.get_account(key).is_none());
    worker.coverage.remove(sub);
    worker.handle_grpc(mirror_update(0)).await.unwrap();
    assert!(engine.get_account(key).is_none());
    worker.coverage.acknowledged(sub);
    engine
        .accounts()
        .store(&[(
            key,
            v42_builder(LOCAL_BALANCE, AccountMode::Missing).slot(NEWER_SLOT).build(),
        )])
        .unwrap();
    // Removal revokes coverage only. Deletion belongs to the eviction caller,
    // so delayed transport cleanup must not delete a newly installed account.
    worker.handle_websocket(Event::Removed(key)).await.unwrap();
    worker
        .handle_websocket(Event::Update {
            sub,
            account: v42_builder(V42_INITIAL, AccountMode::Missing).slot(NEWER_SLOT + 1),
        })
        .await
        .unwrap();
    let account = engine
        .get_account(key)
        .expect("old cleanup must preserve the reacquired mirror");
    assert_eq!(account.data(), LOCAL_BALANCE.to_le_bytes());
    assert_eq!(account.slot(), NEWER_SLOT);
    drop(worker);
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
    let mut worker = Worker::new(&chain_sync);
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
        .mode(AccountMode::Undelegating);
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
    raw.owner = delegation_program_api::id();
    let remote_record = Account {
        owner: delegation_program_api::id(),
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
                    .subscribe(
                        AccountSubscription { pubkey: eata, program: None },
                        Retention::Reuse,
                    )
                    .await
                {
                    Err(WebSocketError::Unavailable) => continue,
                    result => {
                        result?;
                        break;
                    }
                }
            }
            worker.undelegated(eata, PROJECTION_SLOT).await
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
        AccountMode::Mirrored
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

    let mut worker = Worker::new(&chain_sync);
    within(worker.undelegation_requested(fixture.eata, PROJECTION_SLOT - 1))
        .await
        .unwrap();
    assert_eq!(intents(&engine), 0);
    for _ in 0..2 {
        within(worker.undelegation_requested(fixture.eata, PROJECTION_SLOT))
            .await
            .unwrap();
    }
    assert_eq!(intents(&engine), 1);
    assert_eq!(engine.get_account(fixture.eata).unwrap(), raw);
    assert_eq!(
        engine.get_account(fixture.ata).unwrap().mode(),
        AccountMode::Undelegating
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
    scripted(worker.undelegated(fixture.eata, slot), script).await.unwrap();
    assert_eq!(
        engine.get_account(fixture.eata).unwrap().mode(),
        AccountMode::Missing
    );
    let restored = engine.get_account(fixture.ata).unwrap();
    assert_eq!(restored.mode(), AccountMode::Mirrored);
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
        (AccountMode::Missing, false),
        (AccountMode::Mirrored, false),
        (AccountMode::Delegated, true),
        (AccountMode::Magic, true),
        (AccountMode::Undelegating, true),
    ] {
        let key = Pubkey::new_unique();
        engine
            .accounts()
            .store(&[(
                key,
                v42_builder(LOCAL_BALANCE, mode).slot(DELEGATION_SLOT).build(),
            )])
            .unwrap();
        let mut worker = Worker::new(&chain_sync);
        let sub = AccountSubscription { pubkey: key, program: None };
        let generation = worker.coverage.acknowledged(sub);
        worker.coverage.confirmed(0, key, generation);
        within(worker.handle_lost(Source::WebSocket, key)).await.unwrap();
        assert!(engine.get_account(key).is_some());
        within(worker.handle_lost(Source::Grpc, key)).await.unwrap();
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
        assert_eq!(coverage.lost(Source::WebSocket, first.pubkey), (true, None));
        assert_eq!(
            coverage.lost(Source::WebSocket, last.pubkey),
            (true, Some(program))
        );
    }
}

// Direct handler scenarios use the same executor and control handling, without starting another
// transport consumer. The continuous-loop regressions below exercise dispatch and scheduling too.
impl Worker<'_> {
    async fn execute(&mut self, work: Work) -> Completion {
        let (controls, mut requests) = mpsc::unbounded_channel();
        let operations = Operations { sync: self.sync, controls };
        let run = operations.execute(work);
        tokio::pin!(run);
        loop {
            tokio::select! {
                completion = &mut run => return completion,
                Some(control) = requests.recv() => self.control(control),
            }
        }
    }

    async fn drain(&mut self) -> Result<()> {
        // These direct-handler assertions need completion, not a second concurrent event loop.
        while let Some(work) = self.scheduler.next() {
            let completion = self.execute(work).await;
            match self.complete(completion) {
                Ok(()) => {}
                Err(RunError::Operation(error)) => return Err(error),
                Err(error) => panic!("unexpected scheduler failure: {error}"),
            }
        }
        Ok(())
    }

    async fn handle_grpc(&mut self, event: grpc::Event) -> Result<()> {
        self.on_grpc(event).unwrap();
        self.drain().await
    }

    async fn handle_websocket(&mut self, event: Event) -> Result<()> {
        self.on_websocket(event).unwrap();
        self.drain().await
    }

    async fn handle_lost(&mut self, source: Source, pubkey: Pubkey) -> Result<()> {
        self.lost(source, pubkey).unwrap();
        self.drain().await
    }

    async fn undelegated(&mut self, pubkey: Pubkey, slot: u64) -> Result<()> {
        self.execute(Work::Undelegated { pubkey, slot }).await.result
    }

    async fn undelegation_requested(&mut self, pubkey: Pubkey, slot: u64) -> Result<()> {
        self.execute(Work::UndelegationRequested { pubkey, slot }).await.result
    }
}

/// Real subscriptions progress through a full transport queue during both lifecycle and caller acquisition.
#[tokio::test]
async fn event_loop_drains_while_acquisition_or_leases_are_pending() {
    use crate::{AccountProperty, ChainSyncAccount, http::Fetcher, websocket::Pool};
    use nucleus::testkit::v42_sum;

    for lifecycle in [false, true] {
        let mut engine = TestEngine::new().await;
        let mut http = Server::new().await;
        let mut ws = websocket::Server::new().await;
        let remote = yellowstone::Server::new().await;
        let config = crate::support::config(&http.endpoint, Some(&ws), &[&remote.endpoint]);
        let (websocket, websocket_rx) =
            Pool::with_event_capacity(config.websocket, engine.shutdown(), 1);
        let fetcher = Fetcher::new(config.http, engine.clone()).unwrap();
        let (events, grpc_rx) = mpsc::channel(16);
        let grpc = grpc::Client::new(
            0,
            config.grpc[0].clone(),
            engine.clone(),
            events.clone(),
            engine.shutdown(),
        )
        .unwrap();
        let sync = Arc::new(ChainSync {
            aml: None,
            engine: engine.clone(),
            fetcher,
            websocket,
            grpc: vec![grpc],
        });
        let evictions = engine.accounts().subscribe_evictions().unwrap();
        let mut keys = [Pubkey::new_unique(), Pubkey::new_unique()];
        keys.sort_unstable();
        let root = Pubkey::new_unique();
        let mut applied = engine.accounts().subscribe(keys[0]).await;
        let mut activated = engine.accounts().subscribe(root).await;
        let base = keys
            .map(|key| Account::from_builder(key, &v42_builder(V42_INITIAL, AccountMode::Missing)));
        let updated =
            Account::from_builder(keys[0], &v42_builder(LOCAL_BALANCE, AccountMode::Missing));
        let (_, record) = delegation_images(
            root,
            V42_ID,
            engine.authority(),
            DELEGATION_SLOT,
            &[v42_sum(root, &keys)],
        );
        let delegation = Delegation {
            pubkey: root,
            account: v42_builder(V42_INITIAL, AccountMode::Delegated).slot(DELEGATION_SLOT),
            record: record.data,
            source_program: V42_ID,
        };
        let requests = keys.map(|pubkey| ChainSyncAccount {
            pubkey,
            property: AccountProperty::Readonly,
        });
        let mut worker = Worker::new(&sync);
        let scenario = async {
            ws.wait_for_connection().await;
            let acquire = async {
                if lifecycle {
                    events.send(grpc::Event::Delegated(delegation)).await.unwrap();
                    loop {
                        if activated.recv().await.unwrap().mode() == AccountMode::Delegated {
                            break;
                        }
                    }
                } else {
                    crate::support::acquire(&sync, &requests).await.unwrap();
                }
            };
            let providers = async {
                ws.next().await.ack(FIRST_SUBSCRIPTION);
                let call = ws.next().await;
                assert_eq!(call.pubkey, keys[1]);
                // More updates than the one-slot event queue can hold precede the second ACK.
                // The caller holds both leases, or the lifecycle barrier is still running.
                call.ack_with_updates(
                    FIRST_SUBSCRIPTION + 1,
                    &[
                        (FIRST_SUBSCRIPTION, DELEGATION_SLOT + 1, &updated),
                        (FIRST_SUBSCRIPTION, DELEGATION_SLOT + 2, &updated),
                        (FIRST_SUBSCRIPTION, DELEGATION_SLOT + 3, &updated),
                    ],
                );
                http.next().await.respond_accounts(
                    DELEGATION_SLOT,
                    &[Some(base[0].clone()), Some(base[1].clone())],
                );
            };
            tokio::join!(acquire, providers);
            loop {
                let image = applied.recv().await.unwrap();
                if image.slot() == DELEGATION_SLOT + 3 {
                    assert_eq!(image.data(), LOCAL_BALANCE.to_le_bytes());
                    break;
                }
            }
        };
        within(async {
            tokio::select! {
                result = worker.run_updates(websocket_rx, grpc_rx, evictions) => panic!("worker stopped: {result:?}"),
                () = scenario => {},
            }
        }).await;
        drop(worker);
        drop(events);
        drop(sync);
        engine.close().await;
        ws.close().await;
        http.close().await;
        remote.close().await;
    }
}

/// FIFO lanes share program destinations, barriers preserve cross-account order, and stale coverage cannot authorize work.
#[tokio::test]
async fn scheduler_ordering_and_deferred_coverage_checks() {
    let mut engine = TestEngine::new().await;
    let sync = start_handler_sync(&mut engine, None);
    let mut worker = Worker::new(&sync);
    let a = store_v42(&engine, LOCAL_BALANCE, AccountMode::Mirrored);
    let b = Pubkey::new_unique();
    let [direct, data] = AccountSubscription::for_account(a);
    let b_sub = AccountSubscription { pubkey: b, program: None };
    for sub in [direct, data, b_sub] {
        worker.coverage.acknowledged(sub);
    }
    let update = |sub, slot| Work::Update {
        ticket: worker.coverage.ticket(UpdateSource::WebSocket, sub).unwrap(),
        update: Update::Mirror(v42_builder(V42_INITIAL, AccountMode::Missing).slot(slot)),
    };
    let mut scheduler = Scheduler::new(8);
    scheduler.admit(update(direct, 1)).unwrap();
    scheduler.admit(update(data, 2)).unwrap();
    scheduler.admit(update(b_sub, 3)).unwrap();
    scheduler.admit(Work::UndelegationRequested { pubkey: a, slot: 4 }).unwrap();
    scheduler.admit(update(b_sub, 5)).unwrap();
    assert_eq!(scheduler.next().unwrap().lane(), Some(a));
    assert_eq!(scheduler.next().unwrap().lane(), Some(b));
    assert!(scheduler.next().is_none());
    scheduler.finish(Some(a));
    assert!(
        matches!(scheduler.next(), Some(Work::Update { update: Update::Mirror(account), .. }) if account.read().slot() == 2)
    );
    scheduler.finish(Some(b));
    assert!(
        scheduler.next().is_none(),
        "the barrier still waits for ProgramData's shared lane"
    );
    scheduler.finish(Some(a));
    assert!(matches!(
        scheduler.next(),
        Some(Work::UndelegationRequested { slot: 4, .. })
    ));
    assert!(scheduler.next().is_none());
    scheduler.finish(None);
    assert!(
        matches!(scheduler.next(), Some(Work::Update { update: Update::Mirror(account), .. }) if account.read().slot() == 5)
    );
    scheduler.finish(Some(b));
    assert_eq!(scheduler.outstanding, 0);

    let obsolete = update(direct, BUFFERED_SLOT);
    worker.coverage.remove(direct);
    worker.coverage.acknowledged(direct);
    worker.execute(obsolete).await.result.unwrap();
    assert_eq!(
        engine.get_account(a).unwrap().data(),
        LOCAL_BALANCE.to_le_bytes()
    );
    // A last-source eviction can already be queued when a fresh subscription restores coverage.
    worker.execute(Work::Evict { pubkey: a, cached: false }).await.result.unwrap();
    assert!(engine.get_account(a).is_some());
    drop(worker);
    drop(sync);
    engine.close().await;
}

/// Outstanding work includes running heads; exceeding the production limit shuts down without waiting for their leases.
#[tokio::test]
async fn outstanding_limit_triggers_coordinated_shutdown() {
    use nucleus::shutdown::{Service, ShutdownManager};
    let mut engine = TestEngine::new().await;
    let sync = start_handler_sync(&mut engine, None);
    let key = Pubkey::new_unique();
    let sub = AccountSubscription { pubkey: key, program: None };
    let mut coverage = Coverage::default();
    coverage.acknowledged(sub);
    let work = || Work::Update {
        ticket: coverage.ticket(UpdateSource::WebSocket, sub).unwrap(),
        update: Update::Mirror(
            v42_builder(V42_INITIAL, AccountMode::Missing).slot(DELEGATION_SLOT),
        ),
    };
    let mut scheduler = Scheduler::new(2);
    scheduler.admit(work()).unwrap();
    assert!(scheduler.next().is_some());
    scheduler.admit(work()).unwrap();
    assert!(matches!(
        scheduler.admit(work()),
        Err(RunError::Capacity { limit: 2 })
    ));
    scheduler.finish(Some(key));
    scheduler.admit(work()).unwrap();

    let lease = engine.account(key).await.unwrap();
    let (ws, ws_rx) = mpsc::channel(1);
    let (_grpc, grpc_rx) = mpsc::channel(1);
    let (_evictions, evictions) = mpsc::unbounded_channel();
    let mut manager = ShutdownManager::default();
    let shutdown = manager.handle(Service::ChainSyncWorker);
    let task = tokio::spawn(ChainSync::run(
        Arc::clone(&sync),
        ws_rx,
        grpc_rx,
        evictions,
        shutdown,
    ));
    ws.send(Event::Acknowledged(sub)).await.unwrap();
    for _ in 0..=WORK_LIMIT {
        ws.send(Event::Update {
            sub,
            account: v42_builder(V42_INITIAL, AccountMode::Missing).slot(DELEGATION_SLOT),
        })
        .await
        .unwrap();
    }
    let reason = within(manager.wait()).await;
    assert!(
        matches!(&reason, ShutdownReason::Error(error) if error.to_string().contains("8192 outstanding operations"))
    );
    within(task).await.unwrap();
    // The held lease is deliberately released only after termination: no backlog drain was needed.
    drop(lease);
    within(manager.terminate()).await;
    drop(sync);
    engine.close().await;
}
