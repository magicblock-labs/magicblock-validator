use rayon::{
    iter::IndexedParallelIterator,
    prelude::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator},
};
use sleipnir_bank::bank::Bank;
use solana_sdk::{
    account::Account, signature::Keypair, signer::Signer, stake_history::Epoch, system_program,
    system_transaction, transaction::SanitizedTransaction,
};

pub fn init_logger() {
    let _ = env_logger::builder().is_test(true).try_init();
}

pub fn create_accounts(num: usize) -> Vec<Keypair> {
    (0..num).into_par_iter().map(|_| Keypair::new()).collect()
}

pub fn create_funded_accounts(bank: &Bank, num: usize) -> Vec<Keypair> {
    assert!(
        num.is_power_of_two(),
        "must be power of 2 for parallel funding tree"
    );
    let accounts = create_accounts(num);

    accounts.par_iter().for_each(|account| {
        bank.store_account(
            &account.pubkey(),
            &Account {
                lamports: 5100,
                data: vec![],
                owner: system_program::id(),
                executable: false,
                rent_epoch: Epoch::MAX,
            },
        );
    });

    accounts
}

pub fn create_transactions(bank: &Bank, num: usize) -> Vec<SanitizedTransaction> {
    let funded_accounts = create_funded_accounts(bank, 2 * num);
    funded_accounts
        .into_par_iter()
        .chunks(2)
        .map(|chunk| {
            let from = &chunk[0];
            let to = &chunk[1];
            system_transaction::transfer(from, &to.pubkey(), 1, bank.last_blockhash())
        })
        .map(SanitizedTransaction::from_transaction_for_tests)
        .collect()
}
