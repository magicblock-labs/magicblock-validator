use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_magic_program_api as magic_program;
use magicblock_program::{validator::validator_authority, MagicContext};
use solana_account::{AccountSharedData, WritableAccount};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use tracing::*;

use crate::errors::{ApiError, ApiResult};

pub(crate) fn fund_account(
    accountsdb: &AccountsDb,
    pubkey: &Pubkey,
    lamports: u64,
) {
    fund_account_with_data(accountsdb, pubkey, lamports, 0);
}

pub(crate) fn fund_account_with_data(
    accountsdb: &AccountsDb,
    pubkey: &Pubkey,
    lamports: u64,
    size: usize,
) {
    if accountsdb.get_account(pubkey).is_some() {
        return;
    }
    let account = AccountSharedData::new(lamports, size, &Default::default());
    let _ = accountsdb.insert_account(pubkey, &account);
}

pub(crate) fn init_validator_identity(
    accountsdb: &AccountsDb,
    validator_id: &Pubkey,
) {
    fund_account(accountsdb, validator_id, u64::MAX / 2);
    let mut authority = accountsdb.get_account(validator_id).unwrap();
    authority.set_privileged(true);
    let _ = accountsdb.insert_account(validator_id, &authority);
}

pub(crate) fn fund_magic_context(accountsdb: &AccountsDb) {
    const CONTEXT_LAMPORTS: u64 = u64::MAX;

    fund_account_with_data(
        accountsdb,
        &magic_program::MAGIC_CONTEXT_PUBKEY,
        CONTEXT_LAMPORTS,
        MagicContext::SIZE,
    );
    let mut magic_context = accountsdb
        .get_account(&magic_program::MAGIC_CONTEXT_PUBKEY)
        .expect("magic context should have been created");
    magic_context.set_delegated(true);
    magic_context.set_owner(magic_program::ID);

    let _ = accountsdb
        .insert_account(&magic_program::MAGIC_CONTEXT_PUBKEY, &magic_context);
}

pub(crate) fn fund_ephemeral_vault(accountsdb: &AccountsDb) {
    let lamports = Rent::default().minimum_balance(0);
    fund_account(accountsdb, &magic_program::EPHEMERAL_VAULT_PUBKEY, lamports);
    let mut vault = accountsdb
        .get_account(&magic_program::EPHEMERAL_VAULT_PUBKEY)
        .expect("vault should have been created");
    vault.set_ephemeral(true);
    vault.set_owner(magic_program::ID);
    let _ = accountsdb
        .insert_account(&magic_program::EPHEMERAL_VAULT_PUBKEY, &vault);
}

/// Delegates the task scheduler faucet to this validator on the base chain if
/// it is not already delegated. Delegating gives the faucet real (base-chain
/// backed) lamports usable inside the ephemeral rollup, unlike the validator
/// identity. The faucet must already be funded — the validator does not fund
/// it (operators and integration tests airdrop to it).
pub(crate) async fn ensure_faucet_delegated_on_chain(
    rpc_url: String,
    faucet: &Keypair,
) -> ApiResult<()> {
    let validator_keypair = validator_authority();
    let validator_pubkey = validator_keypair.pubkey();
    let faucet_pubkey = faucet.pubkey();

    let rpc =
        RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());

    // A faucet already owned by the delegation program is delegated.
    let already_delegated = matches!(
        rpc.get_account(&faucet_pubkey).await,
        Ok(account) if account.owner == dlp_api::id()
    );
    if already_delegated {
        info!(%faucet_pubkey, "Crank faucet already delegated, skipping");
        return Ok(());
    }

    info!(%faucet_pubkey, "Delegating crank faucet");
    // Hand the on-curve faucet to the delegation program and delegate it to
    // this validator. This makes it a writable, base-chain-backed account
    // inside the ephemeral rollup that can sponsor crank creation (mirrors the
    // on-curve delegation flow used in tests). The faucet must already be
    // funded; the validator does not fund it.
    let assign_ix = solana_system_interface::instruction::assign(
        &faucet_pubkey,
        &dlp_api::id(),
    );
    let delegate_ix = dlp_api::instruction_builder::delegate(
        validator_pubkey,
        faucet_pubkey,
        None,
        dlp_api::args::DelegateArgs {
            commit_frequency_ms: u32::MAX,
            seeds: vec![],
            validator: Some(validator_pubkey),
        },
    );

    let blockhash = rpc.get_latest_blockhash().await.map_err(|err| {
        ApiError::FailedToDelegateFaucet(faucet_pubkey, err.to_string())
    })?;
    let tx = solana_transaction::Transaction::new_signed_with_payer(
        &[assign_ix, delegate_ix],
        Some(&validator_pubkey),
        &[&validator_keypair, faucet],
        blockhash,
    );
    rpc.send_and_confirm_transaction(&tx).await.map_err(|err| {
        ApiError::FailedToDelegateFaucet(faucet_pubkey, err.to_string())
    })?;
    info!(%faucet_pubkey, "Crank faucet delegated");
    Ok(())
}
