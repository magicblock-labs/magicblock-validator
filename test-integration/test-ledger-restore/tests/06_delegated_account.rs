use cleanass::assert_eq;
use std::{path::Path, process::Child};

use integration_test_tools::{expect, tmpdir::resolve_tmp_dir};
use program_flexi_counter::instruction::create_add_ix;
use program_flexi_counter::{
    delegation_program_id,
    instruction::{create_delegate_ix, create_init_ix},
    state::FlexiCounter,
};
use sleipnir_config::ProgramConfig;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer,
};
use test_ledger_restore::{
    cleanup, confirm_tx_with_payer_chain, confirm_tx_with_payer_ephem,
    fetch_counter_chain, fetch_counter_ephem, fetch_counter_owner_chain,
    setup_validator_with_local_remote, wait_for_ledger_persist,
    FLEXI_COUNTER_ID, TMP_DIR_LEDGER,
};

const COUNTER: &str = "Counter of Payer";
fn payer_keypair() -> Keypair {
    Keypair::new()
}

fn get_programs() -> Vec<ProgramConfig> {
    vec![ProgramConfig {
        id: FLEXI_COUNTER_ID.try_into().unwrap(),
        path: "program_flexi_counter.so".to_string(),
    }]
}

#[test]
fn restore_ledger_containing_delegated_account() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer = payer_keypair();

    let (mut validator, _) = write(&ledger_path, &payer);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &payer.pubkey());
    validator.kill().unwrap();
}

fn write(ledger_path: &Path, payer: &Keypair) -> (Child, u64) {
    let programs = get_programs();

    // NOTE: in this test we preload the counter program in the ephemeral instead
    // of relying on it being cloned from the remote
    let (_, mut validator, ctx) =
        setup_validator_with_local_remote(ledger_path, Some(programs), true);

    // Airdrop to payer on chain
    expect!(
        ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    {
        // Create and send init counter instruction on chain
        let ix = create_init_ix(payer.pubkey(), COUNTER.to_string());
        confirm_tx_with_payer_chain(ix, payer, &mut validator);
        let counter = fetch_counter_chain(&payer.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 0,
                updates: 0,
                label: COUNTER.to_string()
            },
            cleanup(&mut validator)
        );
    }
    {
        // Delegate counter to ephemeral
        let ix = create_delegate_ix(payer.pubkey());
        confirm_tx_with_payer_chain(ix, payer, &mut validator);
        let owner = fetch_counter_owner_chain(&payer.pubkey(), &mut validator);
        assert_eq!(owner, delegation_program_id(), cleanup(&mut validator));
    }

    {
        // Increment counter in ephemeral
        let ix = create_add_ix(payer.pubkey(), 3);
        confirm_tx_with_payer_ephem(ix, payer, &mut validator);
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
    (validator, slot)
}

fn read(ledger_path: &Path, payer: &Pubkey) -> Child {
    let programs = get_programs();

    let (_, mut validator, _) =
        setup_validator_with_local_remote(ledger_path, Some(programs), false);

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

    validator
}
