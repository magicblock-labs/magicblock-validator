use integration_test_tools::tmpdir::resolve_tmp_dir;
use integration_test_tools::IntegrationTestContext;
use sleipnir_config::{AccountsConfig, SleipnirConfig};
use sleipnir_config::{LedgerConfig, LifecycleMode};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use std::path::Path;
use std::process::Child;
use test_ledger_restore::start_validator_with_config;

/// Unwraps the provided result and ensures to kill the validator before panicking
/// if the result was an error
macro_rules! expect {
    ($res:expr, $msg:expr, $validator:ident) => {
        match $res {
            Ok(val) => val,
            Err(e) => {
                $validator.kill().unwrap();
                panic!("{}: {:?}", $msg, e);
            }
        }
    };
    ($res:expr, $validator:ident) => {
        match $res {
            Ok(val) => val,
            Err(e) => {
                $validator.kill().unwrap();
                panic!("{:?}", e);
            }
        }
    };
}

fn setup_validator(
    ledger_path: &Path,
    reset: bool,
) -> (Child, IntegrationTestContext) {
    let accounts_config = AccountsConfig {
        lifecycle: LifecycleMode::Offline,
        ..Default::default()
    };

    let config = SleipnirConfig {
        ledger: LedgerConfig {
            reset,
            path: Some(ledger_path.display().to_string()),
        },
        accounts: accounts_config.clone(),
        ..Default::default()
    };
    let Some(validator) = start_validator_with_config(config) else {
        panic!("validator should set up correctly");
    };

    let ctx = IntegrationTestContext::new_ephem_only();
    (validator, ctx)
}

#[test]
fn restore_ledger_with_airdropped_account() {
    let (_, ledger_path) = resolve_tmp_dir("TMP_DIR_LEDGER");

    let pubkey = Pubkey::new_unique();

    // 1. Launch a validator and airdrop to an account
    let (airdrop_sig, slot) = {
        let (mut validator, ctx) = setup_validator(&ledger_path, true);

        let sig = expect!(ctx.airdrop_ephem(&pubkey, 1_111_111), validator);

        let lamports =
            expect!(ctx.fetch_ephem_account_balance(&pubkey), validator);
        assert_eq!(lamports, 1_111_111);

        let slot = ctx.wait_for_next_slot_ephem().unwrap();

        validator.kill().unwrap();
        (sig, slot)
    };

    // 2. Launch another validator reusing ledger
    {
        let (mut validator, ctx) = setup_validator(&ledger_path, false);
        assert!(ctx.wait_for_slot_ephem(slot).is_ok());

        let acc = expect!(ctx.ephem_client.get_account(&pubkey), validator);
        assert_eq!(acc.lamports, 1_111_111);

        let status = ctx
            .ephem_client
            .get_signature_status(&airdrop_sig)
            .unwrap()
            .unwrap();
        assert!(status.is_ok());

        validator.kill().unwrap();
    }
}

fn two_airdropped_accounts_write(
    ledger_path: &Path,
    pubkey1: &Pubkey,
    pubkey2: &Pubkey,
) -> (Child, Signature, Signature, u64) {
    let (mut validator, ctx) = setup_validator(ledger_path, true);

    ctx.wait_for_slot_ephem(5).unwrap();
    let sig1 = expect!(ctx.airdrop_ephem(pubkey1, 1_111_111), validator);

    ctx.wait_for_slot_ephem(10).unwrap();
    let sig2 = expect!(ctx.airdrop_ephem(pubkey2, 2_222_222), validator);

    let lamports1 =
        expect!(ctx.fetch_ephem_account_balance(pubkey1), validator);
    assert_eq!(lamports1, 1_111_111);

    let lamports2 =
        expect!(ctx.fetch_ephem_account_balance(pubkey2), validator);
    assert_eq!(lamports2, 2_222_222);

    let slot = ctx.wait_for_next_slot_ephem().unwrap();

    (validator, sig1, sig2, slot)
}

fn two_airdropped_accounts_read(
    ledger_path: &Path,
    pubkey1: &Pubkey,
    pubkey2: &Pubkey,
    airdrop_sig1: Option<&Signature>,
    airdrop_sig2: Option<&Signature>,
    slot: u64,
) -> Child {
    let (mut validator, ctx) = setup_validator(ledger_path, false);
    assert!(ctx.wait_for_slot_ephem(slot).is_ok());

    let acc1 = expect!(ctx.ephem_client.get_account(pubkey1), validator);
    assert_eq!(acc1.lamports, 1_111_111);

    let acc2 = expect!(ctx.ephem_client.get_account(pubkey2), validator);
    assert_eq!(acc2.lamports, 2_222_222);

    if let Some(sig) = airdrop_sig1 {
        let status =
            ctx.ephem_client.get_signature_status(sig).unwrap().unwrap();
        assert!(status.is_ok());
    }

    if let Some(sig) = airdrop_sig2 {
        let status =
            ctx.ephem_client.get_signature_status(sig).unwrap().unwrap();
        assert!(status.is_ok());
    }
    validator
}

#[test]
fn restore_ledger_with_two_airdropped_accounts() {
    let (_, ledger_path) = resolve_tmp_dir("TMP_DIR_LEDGER");

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();

    let (mut validator, airdrop_sig1, airdrop_sig2, slot) =
        two_airdropped_accounts_write(&ledger_path, &pubkey1, &pubkey2);
    validator.kill().unwrap();

    let mut validator = two_airdropped_accounts_read(
        &ledger_path,
        &pubkey1,
        &pubkey2,
        Some(&airdrop_sig1),
        Some(&airdrop_sig2),
        slot,
    );
    validator.kill().unwrap();
}

#[test]
fn restore_try_write() {
    let (_, ledger_path) = resolve_tmp_dir("TMP_DIR_LEDGER");

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();

    let (mut validator, airdrop_sig1, airdrop_sig2, slot) =
        two_airdropped_accounts_write(&ledger_path, &pubkey1, &pubkey2);

    eprintln!("{}", ledger_path.display());
    eprintln!("{}: {:?}", pubkey1, airdrop_sig1);
    eprintln!("{}: {:?}", pubkey2, airdrop_sig2);
    eprintln!("slot: {}", slot);

    validator.kill().unwrap();
}

/*
1111111QLbz7JHiBTspS962RLKV8GndWFwiEaqKM: 5oQw9R7tabnvVtviJV4zYHpDN1zheaZ1hEDuLwRrKYLHVo7fpuNa7YCTX25hURCyGhWpHFYZujHnPzSbdzSNMF8g
1111111ogCyDbaRMvkdsHB3qfdyFYaG1WtRUAfdh: 2xjWo4wbzGuJ7NqBWSKmtuAsfL8X9zdpzBTkbxZF2U5vEKWjJn1HytrRabfYxCKJYAui9Y42nuhxp9Ufpv5gRYQ1
slot: 2
1111111QLbz7JHiBTspS962RLKV8GndWFwiEaqKM
1111111ogCyDbaRMvkdsHB3qfdyFYaG1WtRUAfdh
*/

#[test]
fn restore_try_read() {
    let (_, ledger_path) = resolve_tmp_dir("TMP_DIR_LEDGER");

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();

    eprintln!("{}", ledger_path.display());
    eprintln!("{}", pubkey1);
    eprintln!("{}", pubkey2);

    let slot = 10;
    let (mut validator, ctx) = setup_validator(&ledger_path, false);
    assert!(ctx.wait_for_slot_ephem(slot).is_ok());

    /*
        let _ = two_airdropped_accounts_read(
            &ledger_path,
            &pubkey1,
            &pubkey2,
            None,
            None,
            slot,
        );
    */
}
