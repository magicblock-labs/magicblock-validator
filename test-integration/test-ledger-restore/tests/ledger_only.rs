use integration_test_tools::tmpdir::resolve_tmp_dir;
use integration_test_tools::IntegrationTestContext;
use sleipnir_config::{AccountsConfig, SleipnirConfig};
use sleipnir_config::{LedgerConfig, LifecycleMode};
use solana_sdk::pubkey::Pubkey;
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
    {
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
        expect!(ctx.airdrop_ephem(&pubkey, 1_111_111), validator);

        let acc = expect!(ctx.ephem_client.get_account(&pubkey), validator);
        assert_eq!(acc.lamports, 1_111_111);
        eprintln!("Account: {:?}", acc);

        // Wait for a slot advance at least
        std::thread::sleep(std::time::Duration::from_secs(1));

        validator.kill().unwrap();
    }

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

        // Wait for a slot advance at least
        std::thread::sleep(std::time::Duration::from_secs(1));

        eprintln!("Ledger: {:?}", ledger_path);
        let ctx = IntegrationTestContext::new_ephem_only();
        let acc = expect!(ctx.ephem_client.get_account(&pubkey), validator);
        assert_eq!(acc.lamports, 1_111_111);
    }
}
