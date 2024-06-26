use std::str::FromStr;

use sleipnir_core::magic_program;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{pubkey::Pubkey, signer::Signer, transaction::Transaction};
use triggercommit_client::{verify, TriggerCommitTestContext};
use triggercommit_program::api::trigger_commit_cpi_instruction;

pub fn main() {
    let TriggerCommitTestContext {
        payer,
        committee,
        commitment,
        client,
        blockhash,
    } = TriggerCommitTestContext::new();
    let ix = trigger_commit_cpi_instruction(
        payer.pubkey(),
        committee.pubkey(),
        // Work around the different solana_sdk versions by creating pubkey from str
        Pubkey::from_str(magic_program::MAGIC_PROGRAM_ADDR).unwrap(),
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );

    let sig = tx.get_signature();
    eprintln!("Sending transaction: '{:?}'", sig);
    eprintln!("Payer:     {}", payer.pubkey());
    eprintln!("Committee: {}", committee.pubkey());
    let res = client.send_and_confirm_transaction_with_spinner_and_config(
        &tx,
        commitment,
        RpcSendTransactionConfig {
            skip_preflight: true,
            ..Default::default()
        },
    );

    verify::commit_to_chain_failed_with_invalid_account_owner(res, commitment);

    // Used to verify that test passed
    println!("Success");
}
