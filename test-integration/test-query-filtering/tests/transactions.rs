use anyhow::{Context, Result};
use program_flexi_counter::instruction::create_add_ix;
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_sdk::{signer::Signer, transaction::Transaction};
use solana_transaction_status::{
    EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction, UiMessage,
    UiTransactionEncoding, UiTransactionStatusMeta,
};
use test_query_filtering::{authed_rpc, setup_query_filtering_fixture};

/// Expected visibility across the four per-account access flags exposed by a
/// permission entry.
#[derive(Clone, Copy, Debug)]
struct ExpectedAccess {
    /// Caller can see the underlying account (full body is preserved).
    account: bool,
    /// Caller can see the transaction message (account keys, instructions).
    message: bool,
    /// Caller can see balance changes.
    balances: bool,
    /// Caller can see log messages.
    logs: bool,
}

/// `getTransaction` honors each visibility flag independently — message,
/// balances, and logs are redacted in isolation; the full-redaction path
/// also wipes the fee.
#[test]
fn query_filtering_redacts_get_transaction_per_flag() -> Result<()> {
    let fixture = setup_query_filtering_fixture()?;

    // The restricted owner sends a tx through the flexi-counter program so the
    // tx touches the restricted counter PDA. flexi-counter is listed as a
    // permission member, so the admission check allows the submission.
    let ephem_rpc = authed_rpc(&fixture.allowed.token);
    let blockhash = ephem_rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        &[create_add_ix(fixture.restricted_owner.pubkey(), 3)],
        Some(&fixture.restricted_owner.pubkey()),
        &[&fixture.restricted_owner],
        blockhash,
    );
    ephem_rpc.send_and_confirm_transaction(&tx)?;

    // Establish baseline shape from the all-access user so each
    // user's response can be diffed against ground truth.
    let baseline = fetch_transaction(&fixture.allowed.token, &tx)?;
    let baseline_balance_count = expect_meta(&baseline).pre_balances.len();
    assert!(
        baseline_balance_count > 0,
        "baseline tx should report at least one pre-balance entry"
    );
    assert!(
        !expect_account_keys(&baseline).is_empty(),
        "baseline tx should expose account keys"
    );

    // The three redaction paths the validator can take. Per-flag granularity
    // (e.g. logs-only, balances-only, message-only members) is exhaustively
    // exercised by the unit tests in `magicblock-query-filtering`; here we
    // verify each end-to-end path lands the right shape on the wire.
    for (user, label, expected) in [
        (
            &fixture.allowed,
            "allowed (no redaction)",
            ExpectedAccess {
                account: true,
                message: true,
                balances: true,
                logs: true,
            },
        ),
        (
            &fixture.account_only,
            "account_only (selective redaction, all sub-flags off)",
            ExpectedAccess {
                account: true,
                message: false,
                balances: false,
                logs: false,
            },
        ),
        (
            &fixture.denied,
            "denied (full redaction)",
            ExpectedAccess {
                account: false,
                message: false,
                balances: false,
                logs: false,
            },
        ),
    ] {
        let response = fetch_transaction(&user.token, &tx)?;
        assert_expected_access(
            &response,
            baseline_balance_count,
            label,
            expected,
        );
    }

    Ok(())
}

fn fetch_transaction(
    token: &str,
    tx: &Transaction,
) -> Result<EncodedConfirmedTransactionWithStatusMeta> {
    let client = authed_rpc(token);
    poll_for_transaction(&client, &tx.signatures[0].to_string())
}

fn poll_for_transaction(
    client: &RpcClient,
    signature: &str,
) -> Result<EncodedConfirmedTransactionWithStatusMeta> {
    use std::{thread::sleep, time::Duration};

    let signature = signature
        .parse()
        .context("failed to parse signature for getTransaction")?;
    let config = RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::Json),
        commitment: Some(CommitmentConfig::confirmed()),
        max_supported_transaction_version: Some(0),
    };
    let mut last_err = None;
    for _ in 0..50 {
        match client.get_transaction_with_config(&signature, config) {
            Ok(response) => return Ok(response),
            Err(err) => {
                last_err = Some(err);
                sleep(Duration::from_millis(100));
            }
        }
    }
    anyhow::bail!(
        "getTransaction did not return within timeout; last error: {last_err:?}"
    )
}

fn expect_meta(
    response: &EncodedConfirmedTransactionWithStatusMeta,
) -> &UiTransactionStatusMeta {
    response
        .transaction
        .meta
        .as_ref()
        .expect("transaction meta is missing")
}

fn expect_account_keys(
    response: &EncodedConfirmedTransactionWithStatusMeta,
) -> Vec<String> {
    match &response.transaction.transaction {
        EncodedTransaction::Json(tx) => match &tx.message {
            UiMessage::Raw(raw) => raw.account_keys.clone(),
            UiMessage::Parsed(parsed) => parsed
                .account_keys
                .iter()
                .map(|key| key.pubkey.clone())
                .collect(),
        },
        other => panic!(
            "expected json-encoded transaction, got {other:?} (response is shaped wrong)"
        ),
    }
}

fn assert_expected_access(
    response: &EncodedConfirmedTransactionWithStatusMeta,
    baseline_balance_count: usize,
    label: &str,
    expected: ExpectedAccess,
) {
    let account_keys = expect_account_keys(response);
    if expected.message {
        assert!(
            !account_keys.is_empty(),
            "[{label}] expected accountKeys to be preserved"
        );
    } else {
        assert!(
            account_keys.is_empty(),
            "[{label}] expected accountKeys to be redacted; got {account_keys:?}"
        );
    }

    let meta = expect_meta(response);
    let log_messages = Option::<Vec<String>>::from(meta.log_messages.clone());

    if expected.account {
        // Selective redaction preserves the shape of meta — balance arrays
        // keep their length so positional indices stay valid; per-account
        // sub-flags only mask individual entries.
        assert_eq!(
            meta.pre_balances.len(),
            baseline_balance_count,
            "[{label}] selective redaction must keep preBalances sized"
        );
        assert_eq!(
            meta.post_balances.len(),
            baseline_balance_count,
            "[{label}] selective redaction must keep postBalances sized"
        );
        if expected.balances {
            // Every account in this tx is visible (unrestricted or
            // member-with-balance-flag), so no entry should be zeroed.
            assert!(
                meta.pre_balances.iter().any(|balance| *balance != 0),
                "[{label}] expected at least one nonzero pre-balance"
            );
        } else {
            // At least one entry must be zeroed (the restricted counter),
            // but the others (payer, program) keep their balance because
            // they are unrestricted accounts.
            assert!(
                meta.pre_balances.contains(&0),
                "[{label}] expected the restricted account's balance to be zeroed; got {:?}",
                meta.pre_balances
            );
        }

        if expected.logs {
            let logs = log_messages.unwrap_or_else(|| {
                panic!("[{label}] expected log messages to be present")
            });
            assert!(
                !logs.is_empty(),
                "[{label}] expected nonempty log messages"
            );
        } else {
            // logs are wiped if *any* touched account hides them.
            let logs = log_messages.unwrap_or_else(|| {
                panic!(
                    "[{label}] expected empty log array (selective redaction)"
                )
            });
            assert!(
                logs.is_empty(),
                "[{label}] expected logMessages to be cleared; got {logs:?}"
            );
        }
    } else {
        // Full redaction: balance arrays and logs are reset to defaults
        // (TransactionStatusMeta::default()), distinguishing this path from
        // selective redaction which keeps balance arrays sized.
        assert!(
            meta.pre_balances.is_empty(),
            "[{label}] full redaction must empty preBalances"
        );
        assert!(
            meta.post_balances.is_empty(),
            "[{label}] full redaction must empty postBalances"
        );
        assert!(
            log_messages.is_none(),
            "[{label}] full redaction must drop logMessages; got {log_messages:?}"
        );
    }
}

/// `sendTransaction` rejects submissions that touch a restricted account
/// through a non-whitelisted, non-member program — regardless of the signing
/// user's per-account access flags.
#[test]
fn query_filtering_rejects_send_transaction_for_non_member_program(
) -> Result<()> {
    use solana_sdk::{
        instruction::{AccountMeta, Instruction},
        pubkey::Pubkey,
    };

    let fixture = setup_query_filtering_fixture()?;

    // A random program id that is not whitelisted and not a permission member.
    let custom_program = Pubkey::new_unique();
    let ephem_rpc = authed_rpc(&fixture.allowed.token);
    let blockhash = ephem_rpc.get_latest_blockhash()?;

    let ix = Instruction {
        program_id: custom_program,
        accounts: vec![AccountMeta::new(fixture.restricted_counter, false)],
        data: vec![],
    };
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&fixture.allowed.keypair.pubkey()),
        &[&fixture.allowed.keypair],
        blockhash,
    );

    let result = ephem_rpc.send_transaction(&tx);
    let err = result
        .err()
        .context("expected sendTransaction to be rejected, but it succeeded")?;
    let message = err.to_string();
    assert!(
        message.contains("access denied"),
        "expected access-denied error from sendTransaction; got {message}"
    );

    Ok(())
}
