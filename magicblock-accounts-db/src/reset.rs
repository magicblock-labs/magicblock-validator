use std::collections::HashSet;

use magicblock_magic_program_api as magic_program;
use solana_pubkey::Pubkey;
use solana_sdk_ids::{
    address_lookup_table, bpf_loader, bpf_loader_deprecated,
    bpf_loader_upgradeable, compute_budget, config, ed25519_program,
    incinerator, loader_v4, native_loader, secp256k1_program,
    secp256r1_program, stake, system_program, sysvar, vote,
    zk_elgamal_proof_program,
};

pub(crate) fn protected_accounts(validator_id: &Pubkey) -> HashSet<Pubkey> {
    // This is buried in the accounts_db::native_mint module and we do not
    // want to take a dependency on that crate just for this stable ID.
    const NATIVE_SOL_ID: Pubkey =
        Pubkey::from_str_const("So11111111111111111111111111111111111111112");

    let mut accounts = sysvar_accounts()
        .into_iter()
        .chain(native_program_accounts())
        .collect::<HashSet<Pubkey>>();

    accounts.insert(stake::config::ID);
    accounts.insert(NATIVE_SOL_ID);
    accounts.insert(magic_program::ID);
    accounts.insert(magic_program::CRANK_PROGRAM_ID);
    accounts.insert(magic_program::CALLBACK_PROGRAM_ID);
    accounts.insert(magic_program::POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID);
    accounts.insert(magic_program::MAGIC_CONTEXT_PUBKEY);
    accounts.insert(magic_program::EPHEMERAL_VAULT_PUBKEY);
    accounts.insert(*validator_id);
    accounts
}

fn sysvar_accounts() -> HashSet<Pubkey> {
    let mut accounts = HashSet::new();
    accounts.insert(sysvar::ID);
    accounts.insert(sysvar::clock::ID);
    accounts.insert(sysvar::epoch_rewards::ID);
    accounts.insert(sysvar::epoch_schedule::ID);
    accounts.insert(sysvar::instructions::ID);
    accounts.insert(sysvar::fees::ID);
    accounts.insert(sysvar::last_restart_slot::ID);
    accounts.insert(sysvar::recent_blockhashes::ID);
    accounts.insert(sysvar::rent::ID);
    accounts.insert(sysvar::rewards::ID);
    accounts.insert(sysvar::slot_hashes::ID);
    accounts.insert(sysvar::slot_history::ID);
    accounts.insert(sysvar::stake_history::ID);
    accounts
}

fn native_program_accounts() -> HashSet<Pubkey> {
    let mut accounts = HashSet::new();
    accounts.insert(address_lookup_table::ID);
    accounts.insert(bpf_loader::ID);
    accounts.insert(bpf_loader_upgradeable::ID);
    accounts.insert(bpf_loader_deprecated::ID);
    accounts.insert(compute_budget::ID);
    accounts.insert(config::ID);
    accounts.insert(ed25519_program::ID);
    accounts.insert(incinerator::ID);
    accounts.insert(loader_v4::ID);
    accounts.insert(native_loader::ID);
    accounts.insert(secp256k1_program::ID);
    accounts.insert(secp256r1_program::ID);
    accounts.insert(stake::ID);
    accounts.insert(system_program::ID);
    accounts.insert(vote::ID);
    accounts.insert(zk_elgamal_proof_program::ID);
    accounts
}
