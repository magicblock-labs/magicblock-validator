use std::{path::Path, process::Child};

use cleanass::assert_eq;
use integration_test_tools::{
    expect, loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
    validator::cleanup,
};
use log::*;
use program_flexi_counter::{
    instruction::{
        create_add_counter_ix, create_add_ix, create_delegate_ix,
        create_init_ix,
    },
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use test_kit::init_logger;
use test_ledger_restore::{
    assert_counter_state, confirm_tx_with_payer_chain,
    confirm_tx_with_payer_ephem, delegate_accounts, fetch_counter_chain,
    fetch_counter_ephem, get_programs_with_flexi_counter,
    setup_validator_with_local_remote, wait_for_cloned_accounts_hydration,
    wait_for_ledger_persist, Counter, State, TMP_DIR_LEDGER,
};

const COUNTER_MAIN: &str = "Main Counter";
const COUNTER_READONLY: &str = "Readonly Counter";
fn payer_keypair() -> Keypair {
    Keypair::new()
}

// In this test we work with two PDAs.
// - Main PDA owned by main payer, delegated to the ephemeral
// - Readonly PDA that is not delegated
//
// ## Writing Ledger
//
// We perform the following actions while the validator is running:
//
// 1. Init Main PDA and delegate it to the ephemeral and add 2
// 2. Init Readonly PDA and add 3
// 3. Run instruction that adds the Readonly count to Main PDA count
//
// At this point the accounts have the following state:
//   - main     chain: count: 0, updates: 0; ephem: updates: 2, count: 5;
//   - readonly chain: count: 3, updates: 1; ephem: count: 3, updates: 1;
//
// 4. We stop the validator and add 1 to the readonly account on chain such
//    that it has the following state:
//    - readonly chain: count: 4, updates: 2; ephem: count: 3, updates: 1;
//
// ## Reading Ledger
//
// Then we restart the validator and first verify that the accounts
// have the correct states and that the readonly account was recloned
// during the hydration phase:
//   - main     chain: count: 0, updates: 0; ephem: count: 5, updates: 2;
//   - readonly chain: count: 4, updates: 2; ephem: count: 4, updates: 2;
//
// We add the readonly to the main account and verify that the main account's
// state is updated as expected:
//   - main     chain: count: 0, updates: 0; ephem: count: 9, updates: 3;
//
// Finally we add 1 to the Readonly PDA on mainnet which should trigger
// a subscription update.
//
// We verify this by doing the following:
// 1. Run instruction that adds Readonly count to Main PDA count
// 2. Verify that the accounts now have the following states:
//   - main     chain: count: 0, updates: 0; ephem: count: 14, updates: 4;
//   - readonly chain: count: 5, updates: 3; ephem: count: 5, updates: 3;

// -----------------
// Helpers
// -----------------
struct ExpectedCounterStates<'a> {
    main: Counter<'a>,
    readonly: Counter<'a>,
}

// NOTE: these are macros to have assertion failures show location inside test code
macro_rules! add_to_readonly {
    ($validator:expr, $payer_readonly:expr, $count:expr, $expected:expr) => {
        let ix = create_add_ix($payer_readonly.pubkey(), $count);
        confirm_tx_with_payer_chain(ix, $payer_readonly, $validator);

        let counter_readonly_chain =
            fetch_counter_chain(&$payer_readonly.pubkey(), $validator);
        assert_eq!(counter_readonly_chain, $expected, cleanup($validator));
    };
}

macro_rules! add_readonly_to_main {
    ($ctx:expr, $validator:expr, $payer_main:expr, $payer_readonly:expr, $expected:expr) => {
        let ix = create_add_counter_ix(
            $payer_main.pubkey(),
            $payer_readonly.pubkey(),
        );
        confirm_tx_with_payer_ephem(ix, $payer_main, $ctx, $validator);

        let counter_main_ephem =
            fetch_counter_ephem($ctx, &$payer_main.pubkey(), $validator);
        assert_eq!(counter_main_ephem, $expected, cleanup($validator));
    };
}

macro_rules! assert_counter_states {
    ($ctx:expr, $validator:expr, $expected:expr) => {{
        assert_counter_state!($ctx, $validator, $expected.main, COUNTER_MAIN);
        assert_counter_state!(
            $ctx,
            $validator,
            $expected.readonly,
            COUNTER_READONLY
        );
    }};
}

// -----------------
// Test
// -----------------
#[ignore = "We would have to hydrate all delegated accounts to support this. We may add this behind a config."]
#[test]
fn test_restore_ledger_using_readonly() {
    init_logger!();
    let (_tmpdir, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer_main = payer_keypair();
    let payer_readonly = payer_keypair();

    let (mut validator, _) = write(&ledger_path, &payer_main, &payer_readonly);
    validator.kill().unwrap();

    // While the validator is down we update the readonly counter on main chain
    add_to_readonly!(
        &mut validator,
        &payer_readonly,
        1,
        FlexiCounter {
            count: 4,
            updates: 2,
            label: COUNTER_READONLY.to_string(),
        }
    );

    let mut validator = read(&ledger_path, &payer_main, &payer_readonly);
    validator.kill().unwrap();
}

fn write(
    ledger_path: &Path,
    payer_main: &Keypair,
    payer_readonly: &Keypair,
) -> (Child, u64) {
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        Some(programs),
        true,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    // Airdrop to payers on chain
    expect!(
        ctx.airdrop_chain(&payer_main.pubkey(), LAMPORTS_PER_SOL),
        validator
    );
    expect!(
        ctx.airdrop_chain(&payer_readonly.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    // Create and send init counter instructions on chain
    confirm_tx_with_payer_chain(
        create_init_ix(payer_main.pubkey(), COUNTER_MAIN.to_string()),
        payer_main,
        &mut validator,
    );
    let (counter_main_pda, _) = FlexiCounter::pda(&payer_main.pubkey());
    debug!(
        "✅ Initialized main counter {counter_main_pda} for payer {} on chain",
        payer_main.pubkey()
    );

    confirm_tx_with_payer_chain(
        create_init_ix(payer_readonly.pubkey(), COUNTER_READONLY.to_string()),
        payer_readonly,
        &mut validator,
    );
    let (counter_readonly_pda, _) = FlexiCounter::pda(&payer_readonly.pubkey());
    debug!(
        "✅ Initialized readonly counter {counter_readonly_pda} for payer {} on chain",
        payer_readonly.pubkey()
    );

    // Delegate main counter to ephemeral and add 2
    {
        confirm_tx_with_payer_chain(
            create_delegate_ix(payer_main.pubkey()),
            payer_main,
            &mut validator,
        );
        debug!("✅ Delegated main counter {counter_main_pda} on chain");

        // Delegate main payer so we can use it in ephem
        delegate_accounts(&ctx, &mut validator, &[payer_main]);
        debug!(
            "✅ Delegated main payer {} for ephem use",
            payer_main.pubkey()
        );

        let ix = create_add_ix(payer_main.pubkey(), 2);
        confirm_tx_with_payer_ephem(ix, payer_main, &ctx, &mut validator);
        debug!("✅ Added 2 to main counter {counter_main_pda} in ephem");

        assert_counter_state!(
            &ctx,
            &mut validator,
            Counter {
                payer: &payer_main.pubkey(),
                chain: State {
                    count: 0,
                    updates: 0,
                },
                ephem: State {
                    count: 2,
                    updates: 1,
                },
            },
            COUNTER_MAIN
        );
    }

    // Add 3 to Readonly Counter on chain
    add_to_readonly!(
        &mut validator,
        payer_readonly,
        3,
        FlexiCounter {
            count: 3,
            updates: 1,
            label: COUNTER_READONLY.to_string(),
        }
    );
    debug!("✅ Added 3 to readonly counter {counter_readonly_pda} on chain");

    // Add Readonly Counter to Main Counter
    // At this point readonly counter is cloned into ephemeral
    add_readonly_to_main!(
        &ctx,
        &mut validator,
        payer_main,
        payer_readonly,
        FlexiCounter {
            count: 5,
            updates: 2,
            label: COUNTER_MAIN.to_string(),
        }
    );
    debug!(
        "✅ Added readonly counter {counter_readonly_pda} to main counter {counter_main_pda} in ephem (cloned readonly)"
    );

    assert_counter_states!(
        &ctx,
        &mut validator,
        ExpectedCounterStates {
            main: Counter {
                payer: &payer_main.pubkey(),
                chain: State {
                    count: 0,
                    updates: 0,
                },
                ephem: State {
                    count: 5,
                    updates: 2,
                },
            },
            readonly: Counter {
                payer: &payer_readonly.pubkey(),
                chain: State {
                    count: 3,
                    updates: 1,
                },
                ephem: State {
                    count: 3,
                    updates: 1,
                },
            },
        }
    );

    debug!("✅ Verified counter states before shutdown");

    let slot = wait_for_ledger_persist(&ctx, &mut validator);
    (validator, slot)
}

fn read(
    ledger_path: &Path,
    payer_main_kp: &Keypair,
    payer_readonly_kp: &Keypair,
) -> Child {
    let payer_main = &payer_main_kp.pubkey();
    let payer_readonly = &payer_readonly_kp.pubkey();
    let programs = get_programs_with_flexi_counter();

    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        Some(programs),
        false,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    wait_for_cloned_accounts_hydration();

    let (counter_main_pda, _) = FlexiCounter::pda(payer_main);
    let (counter_readonly_pda, _) = FlexiCounter::pda(payer_readonly);

    assert_counter_states!(
        &ctx,
        &mut validator,
        ExpectedCounterStates {
            main: Counter {
                payer: payer_main,
                chain: State {
                    count: 0,
                    updates: 0,
                },
                ephem: State {
                    count: 5,
                    updates: 2,
                },
            },
            readonly: Counter {
                payer: payer_readonly,
                chain: State {
                    count: 4,
                    updates: 2,
                },
                // The readonly counter should have gotten cloned into the
                // ephemeral again thus we should see the latest value from chain
                ephem: State {
                    count: 4,
                    updates: 2,
                },
            },
        }
    );
    debug!("✅ Verified counter states after restore");

    // We use it to add to the main counter to ensure that its latest state is used
    add_readonly_to_main!(
        &ctx,
        &mut validator,
        payer_main_kp,
        payer_readonly_kp,
        FlexiCounter {
            count: 9,
            updates: 3,
            label: COUNTER_MAIN.to_string(),
        }
    );
    debug!(
        "✅ Added readonly counter {counter_readonly_pda} to main counter {counter_main_pda} in ephem"
    );

    assert_counter_states!(
        &ctx,
        &mut validator,
        ExpectedCounterStates {
            main: Counter {
                payer: payer_main,
                chain: State {
                    count: 0,
                    updates: 0,
                },
                ephem: State {
                    count: 9,
                    updates: 3,
                },
            },
            readonly: Counter {
                payer: payer_readonly,
                chain: State {
                    count: 4,
                    updates: 2,
                },
                ephem: State {
                    count: 4,
                    updates: 2,
                },
            },
        }
    );

    debug!("✅ Verified counter states after adding readonly to main");

    // Now we update the readonly counter on chain and ensure it is cloned
    // again when we use it in another transaction
    add_to_readonly!(
        &mut validator,
        payer_readonly_kp,
        1,
        FlexiCounter {
            count: 5,
            updates: 3,
            label: COUNTER_READONLY.to_string(),
        }
    );
    debug!(
        "✅ Updated readonly counter {counter_readonly_pda} on chain (count: 5, updates: 3)"
    );

    // NOTE: for now the ephem validator keeps the old state of the readonly account
    // since at this point we re-clone lazily. This will be fixed with the new
    // cloning pipeline at which point we need to update this test.
    // However the validator should have noticed via a subscription update that the
    // account changed.

    // Once we execute a transaction with the readonly account it is cloned again.
    // Here we also ensure that we can use the delegated counter to add
    // the updated readonly count to it
    add_readonly_to_main!(
        &ctx,
        &mut validator,
        payer_main_kp,
        payer_readonly_kp,
        FlexiCounter {
            count: 14,
            updates: 4,
            label: COUNTER_MAIN.to_string(),
        }
    );
    debug!(
        "✅ Added updated readonly counter {counter_readonly_pda} to main counter {counter_main_pda} in ephem (re-cloned readonly)"
    );

    assert_counter_states!(
        &ctx,
        &mut validator,
        ExpectedCounterStates {
            main: Counter {
                payer: payer_main,
                chain: State {
                    count: 0,
                    updates: 0,
                },
                ephem: State {
                    count: 14,
                    updates: 4,
                },
            },
            readonly: Counter {
                payer: payer_readonly,
                chain: State {
                    count: 5,
                    updates: 3,
                },
                // The readonly counter should have gotten cloned into the
                // ephemeral again thus we should see the latest value from chain
                ephem: State {
                    count: 5,
                    updates: 3,
                },
            },
        }
    );

    debug!("✅ Verified counter states after adding readonly to main again");

    validator
}
