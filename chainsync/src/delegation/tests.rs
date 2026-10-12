use std::time::Duration;

use hyper::StatusCode;
use keeper::testkit::{V42_ID, v42_builder};
use solana_account::{AccountMode, ReadableAccount};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use super::{Delegation, PreparedDelegation};
use crate::transport::{
    DELEGATION_SLOT, SERVICE_ERROR, UNAVAILABLE, V42_INITIAL, config,
    delegation as delegation_images,
    http::Server,
    magic::{self, intents},
    scripted, start, start_handler_sync, within,
};
use crate::{AmlConfig, Error, aml};

const LOCAL_BALANCE: i64 = 9;

/// AML failures do not rescue; distinct rejected signers and invalid actions each schedule one rescue.
#[tokio::test]
async fn aml_errors_skip_rescue_but_rejected_signers_and_invalid_actions_schedule_it() {
    let mut engine = magic::engine().await;
    let mut http = Server::new().await;
    let mut config = config(&http.endpoint, None, &[&UNAVAILABLE.parse().unwrap()]);
    config.aml = Some(AmlConfig {
        endpoint: http.endpoint.clone(),
        timeout: Duration::from_secs(5),
    });
    let chain_sync = start(&mut engine, config);
    for (status, body) in
        [(StatusCode::SERVICE_UNAVAILABLE, SERVICE_ERROR), (StatusCode::OK, "invalid")]
    {
        let key = Pubkey::new_unique();
        let prepared = PreparedDelegation {
            pubkey: key,
            account: v42_builder(V42_INITIAL, AccountMode::Delegated).slot(DELEGATION_SLOT),
            source_program: V42_ID,
            actions: Ok(Some(vec![Instruction::new_with_bytes(
                V42_ID,
                &[],
                vec![AccountMeta::new(key, true)],
            )])),
        };
        let script = async {
            http.next().await.respond(status, body);
        };
        let result = scripted(chain_sync.materialize_delegation(prepared), script).await;
        assert!(matches!(
            result,
            Err(Error::Aml(aml::Error::Status(_) | aml::Error::Json(_)))
        ));
        assert!(engine.get_account(key).is_none());
        assert_eq!(intents(&engine), 0);
    }

    let key = Pubkey::new_unique();
    let approved_signer = Pubkey::new_unique();
    let action = Instruction::new_with_bytes(
        V42_ID,
        &[],
        vec![
            AccountMeta::new(key, true),
            AccountMeta::new(key, true),
            AccountMeta::new(approved_signer, true),
            AccountMeta::new(Pubkey::new_unique(), false),
        ],
    );
    let prepared = PreparedDelegation {
        pubkey: key,
        account: v42_builder(V42_INITIAL, AccountMode::Delegated).slot(DELEGATION_SLOT),
        source_program: V42_ID,
        // Duplicates within and across actions must still assess each signer once.
        actions: Ok(Some(vec![action.clone(), action])),
    };
    let script = async {
        let mut assessed = Vec::new();
        for _ in 0..2 {
            let call = http.next().await;
            let signer = call.uri.split("pubkey=").nth(1).unwrap().parse::<Pubkey>().unwrap();
            assessed.push(signer);
            let body = if signer == key { r#"{"isRisky":true}"# } else { r#"{"isRisky":false}"# };
            call.respond(StatusCode::OK, body);
        }
        assessed.sort_unstable();
        let mut expected = [key, approved_signer];
        expected.sort_unstable();
        assert_eq!(assessed, expected);
    };
    scripted(chain_sync.materialize_delegation(prepared), script).await.unwrap();
    assert!(!http.pending());
    assert_eq!(
        engine.get_account(key).unwrap().mode(),
        AccountMode::Transient
    );
    assert_eq!(intents(&engine), 1);
    let invalid = Pubkey::new_unique();
    let (_, mut record) =
        delegation_images(invalid, V42_ID, engine.authority(), DELEGATION_SLOT, &[]);
    record.data.push(255);
    let delegation = Delegation {
        pubkey: invalid,
        account: v42_builder(V42_INITIAL, AccountMode::Delegated).slot(DELEGATION_SLOT),
        source_program: V42_ID,
        record: record.data,
    };
    let (prepared, dependencies) = chain_sync.prepare_delegation(delegation).unwrap();
    assert!(dependencies.is_empty());
    assert!(matches!(&prepared.actions, Err(Error::Actions(_))));
    within(chain_sync.materialize_delegation(prepared)).await.unwrap();
    assert_eq!(
        engine.get_account(invalid).unwrap().mode(),
        AccountMode::Transient
    );
    assert_eq!(intents(&engine), 2);
    drop(chain_sync);
    engine.close().await;
    http.close().await;
}

/// Newer, confined, and same-slot Transient state suppress obsolete rescue without scheduling an intent.
#[tokio::test]
async fn newer_confined_and_transient_state_suppress_obsolete_rescue() {
    let mut engine = magic::engine().await;
    let chain_sync = start_handler_sync(&mut engine, None);
    for (mode, slot) in [
        (AccountMode::Delegated, DELEGATION_SLOT + 1),
        (AccountMode::Magic, DELEGATION_SLOT - 1),
        (AccountMode::Transient, DELEGATION_SLOT),
    ] {
        let key = Pubkey::new_unique();
        engine
            .accounts()
            .store(&[(key, v42_builder(LOCAL_BALANCE, mode).slot(slot).build())])
            .unwrap();
        within(chain_sync.rescue_delegation(
            key,
            v42_builder(V42_INITIAL, AccountMode::Delegated).slot(DELEGATION_SLOT),
            V42_ID,
        ))
        .await
        .unwrap();
        assert_eq!(engine.get_account(key).unwrap().mode(), mode);
        assert_eq!(
            engine.get_account(key).unwrap().data(),
            LOCAL_BALANCE.to_le_bytes()
        );
    }
    assert_eq!(intents(&engine), 0);
    drop(chain_sync);
    engine.close().await;
}
