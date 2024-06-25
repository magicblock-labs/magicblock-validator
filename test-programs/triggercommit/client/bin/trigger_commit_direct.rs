use std::str::FromStr;

use sleipnir_core::magic_program;
use solana_rpc_client::rpc_client::{RpcClient, SerializableTransaction};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::Keypair,
    signer::{SeedDerivable, Signer},
    transaction::Transaction,
};

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
    // Account needs to exist to be commitable
    client
        .request_airdrop(&committee.pubkey(), LAMPORTS_PER_SOL)
        .unwrap();

    let blockhash = client.get_latest_blockhash().unwrap();
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
    eprintln!("{:?}", res);
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
