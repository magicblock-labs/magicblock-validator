#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! HTTP and streamed delegation share canonical ATA projection and confirmed-release behavior.

mod support;

use chainsync::{AccountProperty, ChainSyncAccount};
use dlp_api::compact::ClearText;
use engine::testkit::TestEngine;
use keeper::testkit::store_v42;
use magicblock_chainsync as chainsync;
use magicblock_core::token_programs::EATA_PROGRAM_ID;
use nucleus::testkit::v42_sum;
use solana_account::{AccountBuilder, AccountMode, AccountSharedData, ReadableAccount};
use solana_program_pack::Pack;
use spl_token::state::Account as TokenAccount;
use support::{
    Account, Ata, DELEGATED_TOKEN_BALANCE, DELEGATION_SLOT, FIRST_SUBSCRIPTION, READY_SLOT,
    TOKEN_BALANCE, acquire, http, scripted, start_chain_sync, websocket, within, yellowstone,
};

/// HTTP and gRPC actions read the retained raw eATA before release subscribes and refreshes both images.
#[tokio::test]
async fn http_and_grpc_delegations_retain_raw_eata_and_refresh_both_images() {
    let mut engine = TestEngine::new().await;
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let chain_sync = start_chain_sync(&mut engine, &http, Some(&ws), &grpc);
    ws.wait_for_connection().await;
    grpc.delegation_barrier(&engine, READY_SLOT).await;

    for via_grpc in [false, true] {
        let slot = if via_grpc { DELEGATION_SLOT + 10 } else { DELEGATION_SLOT };
        let context_slot = if via_grpc { slot - 5 } else { slot + 2 };
        let release_slot = slot + 5;
        let subscription = FIRST_SUBSCRIPTION + 3 * u64::from(via_grpc);
        let mut fixture = Ata::new(spl_token::id(), false, engine.authority(), slot);
        let rent = engine.rent().minimum_balance(TokenAccount::LEN);
        fixture.base = fixture.base.lamports(rent);
        let output = store_v42(&engine, 0, AccountMode::Delegated);
        let actions = vec![v42_sum(output, &[fixture.eata])].cleartext();
        fixture.record.extend(borsh::to_vec(&actions).unwrap());
        let mut logs = engine.transactions().subscribe_logs(fixture.ata).await;
        let [application, record] = fixture.delegation();
        let request = ChainSyncAccount {
            pubkey: fixture.ata,
            property: AccountProperty::Readonly,
        };
        let script = async {
            ws.next().await.ack(subscription);
            http.next().await.respond_accounts(
                context_slot,
                &[Some(Account::from_builder(fixture.ata, &fixture.base))],
            );
            let call = http.next().await;
            assert_eq!(call.keys().collect::<Vec<_>>(), [fixture.eata, record.key]);
            assert_eq!(call.min_context_slot(), Some(context_slot));
            // The gRPC path first acquires an ordinary ATA with absent companions;
            // the HTTP path discovers its eATA delegation in this same acquisition.
            let images = if via_grpc {
                [None, None]
            } else {
                [Some(application.clone()), Some(record.clone())]
            };
            call.respond_accounts(context_slot + 1, &images);
        };
        scripted(acquire(&chain_sync, &[request]), script).await.unwrap();
        if via_grpc {
            let base = engine.get_account(fixture.ata).unwrap();
            assert_eq!(
                TokenAccount::unpack(base.data()).unwrap().amount,
                TOKEN_BALANCE
            );
            grpc.confirmed(slot, &[application.clone(), record.clone()]);
        }
        grpc.delegation_barrier(&engine, slot + 1).await;
        let projected = engine.get_account(fixture.ata).unwrap();
        assert_eq!(projected.mode(), AccountMode::Delegated);
        assert_eq!(projected.slot(), slot);
        assert_eq!(projected.owner(), &spl_token::id());
        assert_eq!(projected.lamports(), rent);
        assert_eq!(
            TokenAccount::unpack(projected.data()).unwrap().amount,
            DELEGATED_TOKEN_BALANCE
        );
        assert_eq!(ws.unsubscribed().await, subscription);
        let raw = engine.get_account(fixture.eata).unwrap();
        assert_eq!(raw.mode(), AccountMode::ReadOnly);
        assert_eq!(raw.owner(), &EATA_PROGRAM_ID);
        assert_eq!(raw.data(), fixture.delegated.read().data());
        // v42 reads the first eight bytes of its operand. Empty raw data would
        // fail activation; this result proves the action saw the backing image.
        let value = i64::from_le_bytes(raw.data()[..8].try_into().unwrap());
        let expected = format!("v42: result={value} -> return_data");
        within(async {
            loop {
                let event = logs.recv().await.unwrap();
                if event.logs.iter().any(|line| line.contains(&expected)) {
                    break;
                }
            }
        })
        .await;

        // Reacquiring an evicted raw eATA must not replay the delegation over
        // a balance already modified by local execution.
        let mut token = TokenAccount::unpack(projected.data()).unwrap();
        token.amount += 1;
        let mut data = projected.data().to_vec();
        TokenAccount::pack(token, &mut data).unwrap();
        let projected: AccountSharedData = AccountBuilder::from(projected).data(data).build();
        engine.accounts().store(&[(fixture.ata, projected.clone())]).unwrap();
        engine
            .account(fixture.eata)
            .await
            .unwrap()
            .into_eviction()
            .unwrap()
            .delete()
            .await
            .unwrap();
        let raw_request = ChainSyncAccount {
            pubkey: fixture.eata,
            property: AccountProperty::Writable,
        };
        let script = async {
            let call = http.next().await;
            assert_eq!(call.keys().collect::<Vec<_>>(), [fixture.eata, record.key]);
            call.respond_accounts(release_slot - 1, &[Some(application), Some(record)]);
        };
        assert_eq!(
            scripted(chain_sync.ensure([raw_request]), script).await.unwrap(),
            2
        );
        assert_eq!(engine.get_account(fixture.ata).unwrap(), projected);
        assert_eq!(
            engine.get_account(fixture.eata).unwrap().mode(),
            AccountMode::ReadOnly
        );

        // Real commit scheduling belongs in RedSuite; retain the actual projection
        // and seed the eligible local boundary before testing the streamed release.
        let transient = AccountBuilder::from(projected).mode(AccountMode::Transient).build();
        engine.accounts().store(&[(fixture.ata, transient)]).unwrap();
        grpc.undelegated(release_slot, fixture.eata, engine.authority());
        let script = async {
            let call = ws.next().await;
            assert_eq!(call.pubkey, fixture.eata);
            call.ack(subscription + 1);
            let call = http.next().await;
            assert_eq!(call.keys().collect::<Vec<_>>(), [fixture.eata]);
            assert_eq!(call.min_context_slot(), Some(release_slot));
            let raw = Account::from_builder(
                fixture.eata,
                &fixture.delegated.clone().owner(EATA_PROGRAM_ID),
            );
            call.respond_accounts(release_slot, &[Some(raw)]);
            let call = ws.next().await;
            assert_eq!(call.pubkey, fixture.ata);
            call.ack(subscription + 2);
            http.next().await.respond_accounts(
                release_slot,
                &[Some(Account::from_builder(fixture.ata, &fixture.base))],
            );
            http.next().await.respond_accounts(release_slot, &[None, None]);
        };
        scripted(grpc.delegation_barrier(&engine, release_slot + 1), script).await;
        let restored = engine.get_account(fixture.ata).unwrap();
        assert_eq!(restored.mode(), AccountMode::ReadOnly);
        assert_eq!(restored.slot(), release_slot);
        assert_eq!(
            TokenAccount::unpack(restored.data()).unwrap().amount,
            TOKEN_BALANCE
        );
        let raw = engine.get_account(fixture.eata).unwrap();
        assert_eq!(raw.mode(), AccountMode::ReadOnly);
        assert_eq!(raw.slot(), release_slot);
        assert_eq!(raw.owner(), &EATA_PROGRAM_ID);

        let mut updates = engine.accounts().subscribe(fixture.eata);
        let mut update = Account::from_builder(fixture.eata, &fixture.delegated);
        update.data[64..72].copy_from_slice(&(DELEGATED_TOKEN_BALANCE + 1).to_le_bytes());
        ws.notify(subscription + 1, release_slot + 2, &update);
        within(async {
            loop {
                let account = updates.recv().await.unwrap();
                if account.slot() == release_slot + 2 {
                    assert_eq!(account.data(), update.data);
                    break;
                }
            }
        })
        .await;
        assert_eq!(engine.get_account(fixture.ata).unwrap(), restored);
        assert!(
            !http.pending(),
            "outer/CPI duplicates must produce only one lookup"
        );
    }
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}

/// A raw primary overlapping an ATA companion releases all leases before installing the shared readonly raw eATA.
#[tokio::test]
async fn raw_eata_and_ata_acquisition_share_readonly_raw_eata_without_deadlock() {
    let mut engine = TestEngine::new().await;
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let chain_sync = start_chain_sync(&mut engine, &http, Some(&ws), &grpc);
    ws.wait_for_connection().await;
    let mut fixture = Ata::new(spl_token::id(), false, engine.authority(), DELEGATION_SLOT);
    fixture.base = fixture.base.lamports(engine.rent().minimum_balance(TokenAccount::LEN));
    let [application, record] = fixture.delegation();
    let base = Account::from_builder(fixture.ata, &fixture.base);
    let requests = [
        ChainSyncAccount {
            pubkey: fixture.eata,
            property: AccountProperty::Writable,
        },
        ChainSyncAccount {
            pubkey: fixture.ata,
            property: AccountProperty::Readonly,
        },
    ];
    let script = async {
        let sub = ws.next().await;
        assert_eq!(sub.pubkey, fixture.ata);
        sub.ack(FIRST_SUBSCRIPTION);
        let call = http.next().await;
        assert_eq!(call.keys().count(), 3);
        let images: Vec<_> = call
            .keys()
            .map(|key| {
                Some(if key == fixture.ata {
                    base.clone()
                } else if key == fixture.eata {
                    application.clone()
                } else {
                    assert_eq!(key, record.key);
                    record.clone()
                })
            })
            .collect();
        call.respond_accounts(DELEGATION_SLOT + 1, &images);
        let call = http.next().await;
        assert_eq!(call.keys().collect::<Vec<_>>(), [fixture.eata, record.key]);
        call.respond_accounts(DELEGATION_SLOT + 2, &[Some(application), Some(record)]);
    };
    scripted(acquire(&chain_sync, &requests), script).await.unwrap();
    assert_eq!(ws.unsubscribed().await, FIRST_SUBSCRIPTION);
    let raw = engine.get_account(fixture.eata).unwrap();
    assert_eq!(raw.mode(), AccountMode::ReadOnly);
    assert_eq!(raw.owner(), &EATA_PROGRAM_ID);
    assert_eq!(raw.slot(), DELEGATION_SLOT + 2);
    assert_eq!(
        engine.get_account(fixture.ata).unwrap().mode(),
        AccountMode::Delegated
    );
    assert_eq!(chain_sync.ensure(requests).await.unwrap(), 0);
    assert!(!http.pending());
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}
