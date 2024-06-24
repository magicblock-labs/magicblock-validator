use solana_rpc_client::rpc_client::{RpcClient, SerializableTransaction};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    commitment_config::CommitmentConfig, native_token::LAMPORTS_PER_SOL,
    signature::Keypair, signer::Signer, transaction::Transaction,
};
use triggercommit_program::api::trigger_commit_cpi_instruction;

pub fn main() {
    let payer = Keypair::new();
    let recvr = Keypair::new();
    let commitment = CommitmentConfig::processed();
    let client = RpcClient::new_with_commitment(
        "http://localhost:8899".to_string(),
        commitment,
    );
    client
        .request_airdrop(&payer.pubkey(), LAMPORTS_PER_SOL * 100)
        .unwrap();

    let blockhash = client.get_latest_blockhash().unwrap();
    let ix = trigger_commit_cpi_instruction(payer.pubkey(), recvr.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    let sig = tx.get_signature();
    eprintln!("Sending transaction: '{:?}'", sig);
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
