use std::path::Path;

use cleanass::assert_eq;
use integration_test_tools::{
    expect, loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
    validator::cleanup,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use test_ledger_restore::{
    setup_validator_with_local_remote,
    setup_validator_with_local_remote_and_authority_override,
    wait_for_ledger_persist, TMP_DIR_LEDGER,
};

#[test]
fn test_replay_clone_with_different_local_validator_authority() {
    let (_tmpdir, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let loaded_accounts = LoadedAccounts::default();
    let original_authority = loaded_accounts.validator_authority();
    let cloned_account = Keypair::new();
    let cloned_pubkey = cloned_account.pubkey();
    let expected_lamports = LAMPORTS_PER_SOL;

    write_clone_to_ledger(&ledger_path, &loaded_accounts, &cloned_account);
    restore_with_replay_authority_override(
        &ledger_path,
        original_authority,
        cloned_pubkey,
        expected_lamports,
    );
}

fn write_clone_to_ledger(
    ledger_path: &Path,
    loaded_accounts: &LoadedAccounts,
    cloned_account: &Keypair,
) {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        false,
        loaded_accounts,
    );

    expect!(
        ctx.airdrop_chain(&cloned_account.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    let account = expect!(
        ctx.try_ephem_client()
            .map_err(|e| anyhow::anyhow!("{}", e))
            .and_then(|client| client
                .get_account(&cloned_account.pubkey())
                .map_err(|e| anyhow::anyhow!("{}", e))),
        validator
    );
    assert_eq!(account.lamports, LAMPORTS_PER_SOL, cleanup(&mut validator));

    wait_for_ledger_persist(&ctx, &mut validator);
    test_ledger_restore::kill_validator(&mut validator);
}

fn restore_with_replay_authority_override(
    ledger_path: &Path,
    original_authority: Pubkey,
    cloned_pubkey: Pubkey,
    expected_lamports: u64,
) {
    let (_, mut validator, ctx) =
        setup_validator_with_local_remote_and_authority_override(
            ledger_path,
            None,
            false,
            original_authority,
        );

    let account = expect!(
        ctx.try_ephem_client()
            .map_err(|e| anyhow::anyhow!("{}", e))
            .and_then(|client| client
                .get_account(&cloned_pubkey)
                .map_err(|e| anyhow::anyhow!("{}", e))),
        validator
    );
    assert_eq!(account.lamports, expected_lamports, cleanup(&mut validator));

    test_ledger_restore::kill_validator(&mut validator);
}
