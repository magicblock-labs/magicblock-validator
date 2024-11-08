use integration_test_tools::tmpdir::resolve_tmp_dir;
use integration_test_tools::IntegrationTestContext;
use sleipnir_config::{AccountsConfig, SleipnirConfig};
use sleipnir_config::{LedgerConfig, LifecycleMode};
use solana_sdk::pubkey::Pubkey;
use solana_transaction_status::UiTransactionEncoding;
use test_ledger_restore::start_validator_with_config;

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

#[test]
fn restore_ledger_with_airdropped_account() {
    let (_, ledger_path) = resolve_tmp_dir("TMP_DIR_LEDGER");

    let accounts_config = AccountsConfig {
        lifecycle: LifecycleMode::Offline,
        ..Default::default()
    };
    let pubkey = Pubkey::new_unique();

    // 1. Launch a validator and airdrop to an account
    let (airdrop_sig, slot) = {
        let config = SleipnirConfig {
            ledger: LedgerConfig {
                reset: true,
                path: Some(ledger_path.display().to_string()),
            },
            accounts: accounts_config.clone(),
            ..Default::default()
        };
        let Some(mut validator) = start_validator_with_config(config) else {
            panic!("validator should set up correctly");
        };

        let ctx = IntegrationTestContext::new_ephem_only();
        let sig = expect!(ctx.airdrop_ephem(&pubkey, 1_111_111), validator);

        let lamports =
            expect!(ctx.fetch_ephem_account_balance(pubkey), validator);
        assert_eq!(lamports, 1_111_111);

        let slot = ctx.wait_for_next_slot_ephem().unwrap();

        validator.kill().unwrap();
        (sig, slot)
    };

    // 2. Launch another validator reusing ledger
    {
        let config = SleipnirConfig {
            ledger: LedgerConfig {
                reset: false,
                path: Some(ledger_path.display().to_string()),
            },
            accounts: accounts_config,
            ..Default::default()
        };
        let Some(mut validator) = start_validator_with_config(config) else {
            panic!("validator should set up correctly");
        };

        let ctx = IntegrationTestContext::new_ephem_only();
        assert!(ctx.wait_for_slot_ephem(slot).is_ok());

        let acc = expect!(ctx.ephem_client.get_account(&pubkey), validator);
        assert_eq!(acc.lamports, 1_111_111);

        let status = ctx
            .ephem_client
            .get_signature_status(&airdrop_sig)
            .unwrap()
            .unwrap();
        assert!(status.is_ok());

        let tx = ctx
            .ephem_client
            .get_transaction(&airdrop_sig, UiTransactionEncoding::Base64)
            .unwrap();

        eprintln!("Transaction: {:?}", tx);
        validator.kill().unwrap();
    }
}
