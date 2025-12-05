use std::collections::HashSet;

use magicblock_magic_program_api::{self as magic_program};
use solana_pubkey::Pubkey;
use solana_sdk_ids::{
    address_lookup_table, bpf_loader, bpf_loader_deprecated,
    bpf_loader_upgradeable, ed25519_program, incinerator, native_loader,
    secp256k1_program, stake, system_program, vote,
};
use solana_sysvar;

pub fn blacklisted_accounts(
    validator_id: &Pubkey,
    faucet_id: &Pubkey,
) -> HashSet<Pubkey> {
    // This is buried in the accounts_db::native_mint module and we don't
    // want to take a dependency on that crate just for this ID which won't change
    const NATIVE_SOL_ID: Pubkey =
        Pubkey::from_str_const("So11111111111111111111111111111111111111112");
    let mut blacklisted_accounts = sysvar_accounts()
        .into_iter()
        .chain(native_program_accounts())
        .collect::<HashSet<Pubkey>>();

    blacklisted_accounts.insert(stake::config::ID);

    blacklisted_accounts.insert(NATIVE_SOL_ID);

    blacklisted_accounts.insert(magic_program::ID);
    blacklisted_accounts.insert(magic_program::MAGIC_CONTEXT_PUBKEY);
    blacklisted_accounts.insert(*validator_id);
    blacklisted_accounts.insert(*faucet_id);
    blacklisted_accounts
}

pub fn sysvar_accounts() -> HashSet<Pubkey> {
    let mut blacklisted_sysvars = HashSet::new();
    blacklisted_sysvars.insert(solana_sdk_ids::sysvar::ID);
    blacklisted_sysvars.insert(solana_sysvar::clock::ID);
    blacklisted_sysvars.insert(solana_sysvar::epoch_rewards::ID);
    blacklisted_sysvars.insert(solana_sysvar::epoch_schedule::ID);
    blacklisted_sysvars.insert(solana_sysvar::fees::ID);
    blacklisted_sysvars.insert(solana_sdk_ids::sysvar::instructions::ID);
    blacklisted_sysvars.insert(solana_sysvar::last_restart_slot::ID);
    blacklisted_sysvars.insert(solana_sysvar::recent_blockhashes::ID);
    blacklisted_sysvars.insert(solana_sysvar::rent::ID);
    blacklisted_sysvars.insert(solana_sysvar::rewards::ID);
    blacklisted_sysvars.insert(solana_sysvar::slot_hashes::ID);
    blacklisted_sysvars.insert(solana_sysvar::slot_history::ID);
    blacklisted_sysvars.insert(solana_sysvar::stake_history::ID);
    blacklisted_sysvars
}

pub fn native_program_accounts() -> HashSet<Pubkey> {
    const NATIVE_TOKEN_PROGRAM_ID: Pubkey =
        solana_pubkey::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

    let mut blacklisted_programs = HashSet::new();
    blacklisted_programs.insert(address_lookup_table::ID);
    blacklisted_programs.insert(bpf_loader::ID);
    blacklisted_programs.insert(bpf_loader_upgradeable::ID);
    blacklisted_programs.insert(bpf_loader_deprecated::ID);
    blacklisted_programs.insert(solana_sdk_ids::compute_budget::ID);
    blacklisted_programs.insert(solana_sdk_ids::config::ID);
    blacklisted_programs.insert(ed25519_program::ID);
    blacklisted_programs.insert(incinerator::ID);
    blacklisted_programs.insert(solana_sdk_ids::loader_v4::ID);
    blacklisted_programs.insert(native_loader::ID);
    blacklisted_programs.insert(secp256k1_program::ID);
    blacklisted_programs.insert(solana_sdk_ids::stake::ID);
    blacklisted_programs.insert(system_program::ID);
    blacklisted_programs.insert(vote::ID);
    blacklisted_programs.insert(NATIVE_TOKEN_PROGRAM_ID);
    blacklisted_programs
}
