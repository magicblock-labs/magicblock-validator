use std::str::FromStr;

use sleipnir_core::magic_program;
use solana_rpc_client::rpc_client::{RpcClient, SerializableTransaction};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signer::Signer,
    transaction::Transaction,
};

use crate::utils::TriggerCommitTestContext;

mod utils;

pub fn main() {
    let TriggerCommitTestContext {
        payer,
        committee,
        commitment,
        client,
        blockhash,
    } = TriggerCommitTestContext::new();
    let ix = create_trigger_commit_ix(
        Pubkey::from_str(magic_program::MAGIC_PROGRAM_ADDR).unwrap(),
        payer.pubkey(),
        committee.pubkey(),
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

    let ctx = TriggerCommitTestContext::new();
    let (chain_sig, ephem_logs) = match res {
        Ok(sig) => {
            let logs =
                ctx.fetch_logs(sig, None).expect("Failed to extract logs");
            let chain_sig = ctx.extract_chain_transaction_signature(&logs);
            (chain_sig, logs)
        }
        Err(err) => {
            eprintln!("{:?}", err);
            return;
        }
    };
    eprintln!("Ephemeral logs: ");
    eprintln!("{:#?}", ephem_logs);

    let chain_sig = chain_sig.unwrap_or_else(|| {
        panic!(
            "Chain transaction signature not found in logs, {:#?}",
            ephem_logs
        )
    });

    let devnet_client = RpcClient::new_with_commitment(
        "https://api.devnet.solana.com".to_string(),
        commitment,
    );

    // Wait for tx on devnet to confirm and then get its logs
    let chain_logs = match ctx
        .confirm_transaction(&chain_sig, Some(&devnet_client))
    {
        Ok(res) => {
            eprintln!("Chain transaction confirmed with success: '{:?}'", res);
            ctx.fetch_logs(chain_sig, Some(&devnet_client))
        }
        Err(err) => panic!("Chain transaction failed to confirm: {:?}", err),
    };

    eprintln!("Chain logs: ");
    eprintln!("{:#?}", chain_logs);

    assert!(chain_logs.is_some());
    assert!(chain_logs
        .unwrap()
        .into_iter()
        .any(|log| { log.contains("failed: Invalid account owner") }));
}

fn create_trigger_commit_ix(
    magic_program_id: Pubkey,
    payer: Pubkey,
    committee: Pubkey,
) -> Instruction {
    let instruction_data = vec![1, 0, 0, 0];
    let account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(committee, false),
    ];
    Instruction::new_with_bytes(
        magic_program_id,
        &instruction_data,
        account_metas,
    )
}
