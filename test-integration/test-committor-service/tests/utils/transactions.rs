use log::{debug, error};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::{
    RpcSendTransactionConfig, RpcTransactionConfig,
};
use solana_sdk::{
    commitment_config::CommitmentConfig,
    native_token::LAMPORTS_PER_SOL,
    signature::{Keypair, Signature, Signer},
    transaction::Transaction,
};

use crate::utils::instructions::{
    init_account_and_delegate_ixs, init_validator_fees_vault_ix,
    InitAccountAndDelegateIxs,
};

#[macro_export]
macro_rules! get_account {
    ($rpc_client:ident, $pubkey:expr, $label:literal, $predicate:expr) => {{
        const GET_ACCOUNT_RETRIES: u8 = 12;

        let mut remaining_tries = GET_ACCOUNT_RETRIES;
        loop {
            let acc = $rpc_client
                .get_account_with_commitment(
                    &$pubkey,
                    CommitmentConfig::confirmed(),
                )
                .await
                .ok()
                .and_then(|acc| acc.value);
            if let Some(acc) = acc {
                if $predicate(&acc, remaining_tries) {
                    break acc;
                }
                remaining_tries -= 1;
                if remaining_tries == 0 {
                    panic!(
                        "{} account ({}) does not match condition after {} retries",
                        $label, $pubkey, GET_ACCOUNT_RETRIES
                    );
                }
                $crate::utils::sleep_millis(800).await;
            } else {
                remaining_tries -= 1;
                if remaining_tries == 0 {
                    panic!(
                        "Unable to get {} account ({}) matching condition after {} retries",
                        $label, $pubkey, GET_ACCOUNT_RETRIES
                    );
                }
                if remaining_tries % 10 == 0 {
                    debug!(
                        "Waiting for {} account ({}) to become available",
                        $label, $pubkey
                    );
                }
                $crate::utils::sleep_millis(800).await;
            }
        }
    }};
    ($rpc_client:ident, $pubkey:expr, $label:literal) => {{
        get_account!($rpc_client, $pubkey, $label, |_: &Account, _: u8| true)
    }};
}

#[allow(dead_code)]
pub async fn tx_logs_contain(
    rpc_client: &RpcClient,
    signature: &Signature,
    needle: &str,
) -> bool {
    // NOTE: we encountered the following error a few times which makes tests fail for the
    //       wrong reason:
    //       Error {
    //          request: Some(GetTransaction),
    //          kind: SerdeJson( Error(
    //              "invalid type: null,
    //              expected struct EncodedConfirmedTransactionWithStatusMeta",
    //              line: 0, column: 0))
    //      }
    //      Therefore we retry a few times.
    const MAX_RETRIES: usize = 5;
    let mut retries = MAX_RETRIES;
    let tx = loop {
        match rpc_client
            .get_transaction_with_config(
                signature,
                RpcTransactionConfig {
                    commitment: Some(CommitmentConfig::confirmed()),
                    max_supported_transaction_version: Some(0),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(tx) => break tx,
            Err(err) => {
                log::error!("Failed to get transaction: {}", err);
                retries -= 1;
                if retries == 0 {
                    panic!(
                        "Failed to get transaction after {} retries",
                        MAX_RETRIES
                    );
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(100))
                    .await;
            }
        };
    };
    let logs = tx
        .transaction
        .meta
        .as_ref()
        .unwrap()
        .log_messages
        .clone()
        .unwrap_or_else(Vec::new);
    logs.iter().any(|log| log.contains(needle))
}

/// This needs to be run for each test that required a new counter to be delegated
pub async fn init_and_delegate_account_on_chain(
    counter_auth: &Keypair,
    bytes: u64,
    label: Option<String>,
) -> (Pubkey, Account) {
    let rpc_client = RpcClient::new("http://localhost:7799".to_string());

    rpc_client
        .request_airdrop(&counter_auth.pubkey(), 777 * LAMPORTS_PER_SOL)
        .await
        .unwrap();
    debug!("Airdropped to counter auth: {} SOL", 777 * LAMPORTS_PER_SOL);

    let InitAccountAndDelegateIxs {
        init: init_counter_ix,
        reallocs: realloc_ixs,
        delegate: delegate_ix,
        pda,
        rent_excempt,
    } = init_account_and_delegate_ixs(counter_auth.pubkey(), bytes, label);

    let latest_block_hash = rpc_client.get_latest_blockhash().await.unwrap();
    // 1. Init account
    rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &Transaction::new_signed_with_payer(
                &[init_counter_ix],
                Some(&counter_auth.pubkey()),
                &[&counter_auth],
                latest_block_hash,
            ),
            CommitmentConfig::confirmed(),
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .await
        .expect("Failed to init account");
    debug!("Init account: {:?}", pda);

    // 2. Airdrop to account for extra rent needed for reallocs
    rpc_client
        .request_airdrop(&pda, rent_excempt)
        .await
        .unwrap();

    debug!(
        "Airdropped to account: {:4} {}SOL to pay rent for {} bytes",
        pda,
        rent_excempt as f64 / LAMPORTS_PER_SOL as f64,
        bytes
    );

    // 3. Run reallocs
    for realloc_ix_chunk in realloc_ixs.chunks(10) {
        let tx = Transaction::new_signed_with_payer(
            realloc_ix_chunk,
            Some(&counter_auth.pubkey()),
            &[&counter_auth],
            latest_block_hash,
        );
        rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .expect("Failed to realloc");
    }
    debug!("Reallocs done");

    // 4. Delegate account
    rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &Transaction::new_signed_with_payer(
                &[delegate_ix],
                Some(&counter_auth.pubkey()),
                &[&counter_auth],
                latest_block_hash,
            ),
            CommitmentConfig::confirmed(),
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .await
        .expect("Failed to delegate");
    debug!("Delegated account: {:?}", pda);
    let pda_acc = get_account!(rpc_client, pda, "pda");

    (pda, pda_acc)
}

/// This needs to be run once for all tests
pub async fn fund_validator_auth_and_ensure_validator_fees_vault(
    validator_auth: &Keypair,
) {
    let rpc_client = RpcClient::new("http://localhost:7799".to_string());
    rpc_client
        .request_airdrop(&validator_auth.pubkey(), 777 * LAMPORTS_PER_SOL)
        .await
        .unwrap();
    debug!("Airdropped to validator: {} ", validator_auth.pubkey(),);

    let validator_fees_vault_exists = rpc_client
        .get_account(&validator_auth.pubkey())
        .await
        .is_ok();

    if !validator_fees_vault_exists {
        let latest_block_hash =
            rpc_client.get_latest_blockhash().await.unwrap();
        let init_validator_fees_vault_ix =
            init_validator_fees_vault_ix(validator_auth.pubkey());
        // If this fails it might be due to a race condition where another test
        // already initialized it, so we can safely ignore the error
        let _ = rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &Transaction::new_signed_with_payer(
                    &[init_validator_fees_vault_ix],
                    Some(&validator_auth.pubkey()),
                    &[&validator_auth],
                    latest_block_hash,
                ),
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(|err| {
                error!("Failed to init validator fees vault: {}", err);
            });
    }
}
