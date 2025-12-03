#![allow(clippy::result_large_err)]

use std::{thread::sleep, time::Duration};

use log::*;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::{
    client_error,
    config::{RpcSendTransactionConfig, RpcSimulateTransactionConfig},
};
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::Instruction,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

use crate::conversions::stringify_simulation_result;

pub fn send_and_confirm_instructions_with_payer(
    rpc_client: &solana_rpc_client::rpc_client::RpcClient,
    ixs: &[Instruction],
    payer: &Keypair,
    commitment: CommitmentConfig,
    label: &str,
) -> Result<(Signature, bool), client_error::Error> {
    debug!(
        "Sending {} with {} instructions, payer: {}",
        label,
        ixs.len(),
        payer.pubkey(),
    );
    let (sig, tx) = send_instructions_with_payer(rpc_client, ixs, payer)?;
    debug!("Confirming transaction with signature: {}", sig);
    confirm_transaction(&sig, rpc_client, commitment, Some(&tx))
        .map(|confirmed| (sig, confirmed))
}

pub fn send_instructions_with_payer(
    rpc_client: &RpcClient,
    ixs: &[Instruction],
    payer: &Keypair,
) -> Result<(Signature, Transaction), client_error::Error> {
    let blockhash = rpc_client.get_latest_blockhash()?;
    let mut tx = Transaction::new_with_payer(ixs, Some(&payer.pubkey()));
    tx.sign(&[payer], blockhash);
    let sig = send_transaction(rpc_client, &mut tx, &[payer], true)?;
    Ok((sig, tx))
}

pub fn send_transaction(
    rpc_client: &RpcClient,
    tx: &mut Transaction,
    signers: &[&Keypair],
    skip_preflight: bool,
) -> Result<Signature, client_error::Error> {
    let blockhash = rpc_client.get_latest_blockhash()?;
    tx.try_sign(signers, blockhash)?;
    let sig = rpc_client.send_transaction_with_config(
        tx,
        RpcSendTransactionConfig {
            skip_preflight,
            ..Default::default()
        },
    )?;
    Ok(sig)
}

pub fn send_and_confirm_transaction(
    rpc_client: &RpcClient,
    tx: &mut Transaction,
    signers: &[&Keypair],
    commitment: CommitmentConfig,
) -> Result<(Signature, bool), client_error::Error> {
    let sig = send_transaction(rpc_client, tx, signers, true)?;
    confirm_transaction(&sig, rpc_client, commitment, Some(tx))
        .map(|confirmed| (sig, confirmed))
}

pub fn confirm_transaction(
    sig: &Signature,
    rpc_client: &RpcClient,
    commitment_config: CommitmentConfig,
    tx: Option<&Transaction>,
) -> Result<bool, client_error::Error> {
    // Allow RPC failures to persist for up to 1 sec
    const MAX_FAILURES: u64 = 5;
    const MILLIS_UNTIL_RETRY: u64 = 200;
    let mut failure_count = 0;

    // Allow transactions to take up to 40 seconds to confirm
    const MAX_UNCONFIRMED_COUNT: u64 = 40;
    const MILLIS_UNTIL_RECONFIRM: u64 = 500;
    const SIMULATE_THRESHOLD: u64 = 5;
    let mut unconfirmed_count = 0;

    loop {
        match rpc_client
            .confirm_transaction_with_commitment(sig, commitment_config)
        {
            Ok(res) if res.value => {
                return Ok(res.value);
            }
            Ok(_) => {
                unconfirmed_count += 1;
                if unconfirmed_count >= MAX_UNCONFIRMED_COUNT {
                    return Ok(false);
                }
                if let Some(tx) = tx {
                    if unconfirmed_count == SIMULATE_THRESHOLD {
                        // After a few tries, simulate the transaction to log helpful
                        // information about while it isn't landing
                        match rpc_client.simulate_transaction_with_config(
                            tx,
                            RpcSimulateTransactionConfig {
                                sig_verify: false,
                                replace_recent_blockhash: true,
                                ..Default::default()
                            },
                        ) {
                            Ok(res) => {
                                warn!(
                                    "{}",
                                    stringify_simulation_result(res.value, sig)
                                );
                            }
                            Err(err) => {
                                warn!(
                                    "Failed to simulate transaction: {:?}",
                                    err
                                );
                            }
                        }
                    }
                }
                sleep(Duration::from_millis(MILLIS_UNTIL_RECONFIRM));
            }
            Err(err) => {
                failure_count += 1;
                if failure_count >= MAX_FAILURES {
                    return Err(err);
                } else {
                    sleep(Duration::from_millis(MILLIS_UNTIL_RETRY));
                }
            }
        }
    }
}
