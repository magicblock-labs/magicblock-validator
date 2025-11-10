use std::collections::HashSet;

use magicblock_magic_program_api as magic_program;
use solana_pubkey::Pubkey;

pub fn blacklisted_accounts(
    validator_id: &Pubkey,
    faucet_id: &Pubkey,
) -> HashSet<Pubkey> {
    // This is buried in the accounts_db::native_mint module and we don't
    // want to take a dependency on that crate just for this ID which won't change
    const NATIVE_SOL_ID: Pubkey =
        solana_sdk::pubkey!("So11111111111111111111111111111111111111112");
    let mut blacklisted_accounts = sysvar_accounts()
        .into_iter()
        .chain(native_program_accounts())
        .collect::<HashSet<Pubkey>>();

    blacklisted_accounts.insert(solana_sdk::stake::config::ID);
    blacklisted_accounts.insert(solana_sdk::feature::ID);

    blacklisted_accounts.insert(NATIVE_SOL_ID);

    blacklisted_accounts.insert(magic_program::ID);
    blacklisted_accounts.insert(magic_program::MAGIC_CONTEXT_PUBKEY);
    blacklisted_accounts.insert(magic_program::TASK_CONTEXT_PUBKEY);
    blacklisted_accounts.insert(*validator_id);
    blacklisted_accounts.insert(*faucet_id);
    blacklisted_accounts
}

pub fn sysvar_accounts() -> HashSet<Pubkey> {
    let mut blacklisted_sysvars = HashSet::new();
    blacklisted_sysvars.insert(solana_sdk::sysvar::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::clock::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::epoch_rewards::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::epoch_schedule::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::fees::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::instructions::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::last_restart_slot::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::recent_blockhashes::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::rent::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::rewards::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::slot_hashes::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::slot_history::ID);
    blacklisted_sysvars.insert(solana_sdk::sysvar::stake_history::ID);
    blacklisted_sysvars
}

pub fn native_program_accounts() -> HashSet<Pubkey> {
    const NATIVE_TOKEN_PROGRAM_ID: Pubkey =
        solana_sdk::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

    let mut blacklisted_programs = HashSet::new();
    blacklisted_programs.insert(solana_sdk::address_lookup_table::program::ID);
    blacklisted_programs.insert(solana_sdk::bpf_loader::ID);
    blacklisted_programs.insert(solana_sdk::bpf_loader_deprecated::ID);
    blacklisted_programs.insert(solana_sdk::bpf_loader_upgradeable::ID);
    blacklisted_programs.insert(solana_sdk::compute_budget::ID);
    blacklisted_programs.insert(solana_sdk::config::program::ID);
    blacklisted_programs.insert(solana_sdk::ed25519_program::ID);
    blacklisted_programs.insert(solana_sdk::incinerator::ID);
    blacklisted_programs.insert(solana_sdk::loader_v4::ID);
    blacklisted_programs.insert(solana_sdk::native_loader::ID);
    blacklisted_programs.insert(solana_sdk::secp256k1_program::ID);
    blacklisted_programs.insert(solana_sdk::stake::program::ID);
    blacklisted_programs.insert(solana_sdk::system_program::ID);
    blacklisted_programs.insert(solana_sdk::vote::program::ID);
    blacklisted_programs.insert(NATIVE_TOKEN_PROGRAM_ID);
    blacklisted_programs
}
