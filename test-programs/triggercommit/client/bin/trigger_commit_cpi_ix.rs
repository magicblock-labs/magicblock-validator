use std::str::FromStr;

use sleipnir_core::magic_program;
use solana_rpc_client::rpc_client::{RpcClient, SerializableTransaction};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::Keypair,
    signer::{SeedDerivable, Signer},
    transaction::Transaction,
};
use triggercommit_program::api::trigger_commit_cpi_instruction;

pub fn main() {
    let payer = Keypair::from_seed(&[2u8; 32]).unwrap();
    let committee = Keypair::from_seed(&[3u8; 32]).unwrap();
    let commitment = CommitmentConfig::processed();

    let client = RpcClient::new_with_commitment(
        "http://localhost:8899".to_string(),
        commitment,
    );
    client
        .request_airdrop(&payer.pubkey(), LAMPORTS_PER_SOL * 100)
        .unwrap();

    let blockhash = client.get_latest_blockhash().unwrap();
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
    eprintln!("{:?}", res);
}
