use anyhow::Context;
use dlp::args::DelegateEphemeralBalanceArgs;
use log::*;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::instruction::Instruction;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

pub async fn top_up_ephemeral_fee_balance(
    rpc_client: &RpcClient,
    payer: &Keypair,
    recvr: Pubkey,
    sol: u64,
    delegate: bool,
) -> anyhow::Result<(Signature, Pubkey, Pubkey)> {
    let topup_ix = dlp::instruction_builder::top_up_ephemeral_balance(
        payer.pubkey(),
        recvr,
        Some(sol * LAMPORTS_PER_SOL),
        None,
    );
    let mut ixs = vec![topup_ix];
    if delegate {
        let delegate_ix = dlp::instruction_builder::delegate_ephemeral_balance(
            payer.pubkey(),
            recvr,
            DelegateEphemeralBalanceArgs::default(),
        );
        ixs.push(delegate_ix);
    }
    let sig = send_instructions(&rpc_client, &ixs, &[payer], "topup ephemeral")
        .await?;
    let (ephemeral_balance_pda, deleg_record) = escrow_pdas(&recvr);
    debug!(
        "Top-up ephemeral balance {} {ephemeral_balance_pda} sig: {sig}",
        payer.pubkey()
    );
    Ok((sig, ephemeral_balance_pda, deleg_record))
}

pub fn escrow_pdas(payer: &Pubkey) -> (Pubkey, Pubkey) {
    let ephemeral_balance_pda = ephemeral_balance_pda_from_payer_pubkey(payer);
    let escrow_deleg_record = delegation_record_pubkey(&ephemeral_balance_pda);
    (ephemeral_balance_pda, escrow_deleg_record)
}

pub fn delegation_record_pubkey(pubkey: &Pubkey) -> Pubkey {
    dlp::pda::delegation_record_pda_from_delegated_account(pubkey)
}

pub fn ephemeral_balance_pda_from_payer_pubkey(payer: &Pubkey) -> Pubkey {
    dlp::pda::ephemeral_balance_pda_from_payer(payer, 0)
}

// -----------------
// Helpers
// -----------------
async fn send_transaction(
    rpc_client: &RpcClient,
    transaction: &Transaction,
    label: &str,
) -> anyhow::Result<Signature> {
    rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            transaction,
            rpc_client.commitment(),
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .await
        .with_context(|| format!("Failed to send and confirm {label}"))
}

async fn send_instructions(
    rpc_client: &RpcClient,
    ixs: &[Instruction],
    signers: &[&Keypair],
    label: &str,
) -> anyhow::Result<Signature> {
    let recent_blockhash = rpc_client
        .get_latest_blockhash()
        .await
        .expect("Failed to get recent blockhash");
    let mut transaction =
        Transaction::new_with_payer(ixs, Some(&signers[0].pubkey()));
    transaction.sign(signers, recent_blockhash);
    send_transaction(rpc_client, &transaction, label).await
}
