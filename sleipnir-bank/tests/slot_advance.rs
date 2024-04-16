#![cfg(feature = "dev-context-only-utils")]

#[allow(unused_imports)]
use log::*;
use sleipnir_bank::bank::Bank;
use solana_sdk::{
    account::Account, genesis_config::create_genesis_config, pubkey::Pubkey,
    system_program,
};
use test_tools_core::init_logger;

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
    // This test ensures that no matter which slot we store an account, we can
    // always get it in that same slot or later slots.
    // This did not work until we properly updated the bank's ancestors when we
    // advanace a slot.
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config, None, None);

    macro_rules! assert_account_stored {
        ($acc: expr) => {
            assert_eq!(
                bank.get_account(&$acc.pubkey).unwrap(),
                $acc.account.clone().into()
            )
        };
    }

    macro_rules! assert_account_not_stored {
        ($acc: expr) => {
            assert!(bank.get_account(&$acc.pubkey).is_none(),)
        };
    }

    let acc0 = create_account(0);
    let acc1 = create_account(1);
    let acc2 = create_account(2);

    assert_account_not_stored!(acc0);
    assert_account_not_stored!(acc1);
    assert_account_not_stored!(acc2);

    // Slot 0
    {
        bank.store_account(&acc0.pubkey, &acc0.account);
        assert_account_stored!(acc0);
        assert_account_not_stored!(acc1);
        assert_account_not_stored!(acc2);
    }

    // Slot 1
    {
        bank.advance_slot();
        bank.store_account(&acc1.pubkey, &acc1.account);

        assert_account_stored!(acc0);
        assert_account_stored!(acc1);
        assert_account_not_stored!(acc2);
    }

    // Slot 2
    {
        bank.advance_slot();
        bank.store_account(&acc2.pubkey, &acc2.account);
        assert_account_stored!(acc0);
        assert_account_stored!(acc1);
        assert_account_stored!(acc2);
    }
    // Slot 3
    {
        bank.advance_slot();
        assert_account_stored!(acc0);
        assert_account_stored!(acc1);
        assert_account_stored!(acc2);
    }
}

#[test]
fn test_bank_advances_slot_in_clock_sysvar() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config, None, None);

    assert_eq!(bank.clock().slot, 0);

    bank.advance_slot();
    assert_eq!(bank.clock().slot, 1);

    bank.advance_slot();
    assert_eq!(bank.clock().slot, 2);

    bank.advance_slot();
    bank.advance_slot();
    bank.advance_slot();
    assert_eq!(bank.clock().slot, 5);
}
