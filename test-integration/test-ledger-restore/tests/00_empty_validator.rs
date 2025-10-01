use std::{path::Path, process::Child};

use integration_test_tools::{
    loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
};
use test_ledger_restore::{
    setup_validator_with_local_remote, wait_for_ledger_persist, TMP_DIR_LEDGER,
};

// Here we test that we can restore a ledger of a validator that did not run any
// transactions. Mainly this can also be used to ensure that no accounts are cloned
// in that case.

#[test]
fn test_restore_ledger_empty_validator() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, _) = write(&ledger_path);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path);
    validator.kill().unwrap();
}

fn write(ledger_path: &Path) -> (Child, u64) {
    // Launch a validator and airdrop to an account
    let (_, mut validator, _) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    let slot = wait_for_ledger_persist(&mut validator);

    validator.kill().unwrap();
    (validator, slot)
}

fn read(ledger_path: &Path) -> Child {
    // Launch another validator reusing ledger
    let (_, validator, _) = setup_validator_with_local_remote(
        ledger_path,
        None,
        false,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    validator
}
