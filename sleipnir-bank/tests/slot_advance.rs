#![cfg(feature = "dev-context-only-utils")]

use sleipnir_bank::bank::Bank;
use sleipnir_bank::bank_dev_utils::init_logger;
use solana_sdk::account::Account;
use solana_sdk::genesis_config::create_genesis_config;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::system_program;

struct AccountWithAddr {
    pub pubkey: Pubkey,
    pub account: Account,
}
fn create_account(slot: u64) -> AccountWithAddr {
    AccountWithAddr {
        pubkey: Pubkey::new_unique(),
        account: Account {
            lamports: 1_000_000 + slot,
            data: vec![],
            owner: system_program::id(),
            executable: false,
            rent_epoch: u64::MAX,
        },
    }
}

#[test]
fn test_bank_store_get_accounts_across_slots() {
    init_logger();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);

    let acc0 = create_account(0);
    let acc1 = create_account(1);
    let acc2 = create_account(2);

    assert!(bank.get_account(&acc0.pubkey).is_none());
    assert!(bank.get_account(&acc1.pubkey).is_none());
    assert!(bank.get_account(&acc2.pubkey).is_none());

    // Slot 0
    {
        bank.store_account(&acc0.pubkey, &acc0.account);
        eprintln!("0: acc0: {:?}", bank.get_account(&acc0.pubkey));
        // assert_eq!(bank.get_account(&acc0.pubkey).unwrap(), acc0.account.into());
        assert!(bank.get_account(&acc1.pubkey).is_none());
        assert!(bank.get_account(&acc2.pubkey).is_none());
    }

    // Slot 1
    {
        bank.advance_slot();
        bank.store_account(&acc1.pubkey, &acc1.account);
        eprintln!("1: acc0: {:?}", bank.get_account(&acc0.pubkey));
        eprintln!("1: acc1: {:?}", bank.get_account(&acc1.pubkey));
        eprintln!("1: acc2: {:?}", bank.get_account(&acc2.pubkey));

        assert!(bank.get_account(&acc2.pubkey).is_none());
    }

    // Slot 2
    {
        bank.advance_slot();
        bank.store_account(&acc2.pubkey, &acc2.account);
        eprintln!("2: acc0: {:?}", bank.get_account(&acc0.pubkey));
        eprintln!("2: acc1: {:?}", bank.get_account(&acc1.pubkey));
        eprintln!("2: acc2: {:?}", bank.get_account(&acc2.pubkey));
    }
}
