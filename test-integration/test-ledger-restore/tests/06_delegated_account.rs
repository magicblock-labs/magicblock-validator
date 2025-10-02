use log::*;
use std::{path::Path, process::Child};

use cleanass::assert_eq;
use integration_test_tools::{
    loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
    validator::cleanup,
};
use program_flexi_counter::{instruction::create_add_ix, state::FlexiCounter};
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use test_kit::init_logger;
use test_ledger_restore::{
    confirm_tx_with_payer_ephem, fetch_counter_ephem,
    init_and_delegate_counter_and_payer, setup_validator_with_local_remote,
    wait_for_cloned_accounts_hydration, wait_for_ledger_persist,
    TMP_DIR_LEDGER,
};

const COUNTER: &str = "Counter of Payer";
#[test]
fn test_restore_ledger_containing_delegated_account() {
    init_logger!();
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, _, payer) = write(&ledger_path);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &payer.pubkey());
    validator.kill().unwrap();
}

fn write(ledger_path: &Path) -> (Child, u64, Keypair) {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    let (payer, _counter) =
        init_and_delegate_counter_and_payer(&ctx, &mut validator, COUNTER);

    {
        // Increment counter in ephemeral
        let ix = create_add_ix(payer.pubkey(), 3);
        confirm_tx_with_payer_ephem(ix, &payer, &mut validator);
        let counter = fetch_counter_ephem(&payer.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 3,
                updates: 1,
                label: COUNTER.to_string()
            },
            cleanup(&mut validator)
        );
    }

    let slot = wait_for_ledger_persist(&mut validator);
    (validator, slot, payer)
}

fn read(ledger_path: &Path, payer: &Pubkey) -> Child {
    let (_, mut validator, _) = setup_validator_with_local_remote(
        ledger_path,
        None,
        false,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    wait_for_cloned_accounts_hydration();

    let counter_decoded = fetch_counter_ephem(payer, &mut validator);
    assert_eq!(
        counter_decoded,
        FlexiCounter {
            count: 3,
            updates: 1,
            label: COUNTER.to_string()
        },
        cleanup(&mut validator)
    );
    debug!("✅ Verified counter state after restore");

    validator
}
