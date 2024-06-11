use sleipnir_bank::bank::Bank;
use solana_sdk::{
    account::Account, clock::Epoch, pubkey::Pubkey, signature::Keypair,
    signer::Signer, system_program,
};

pub(crate) fn fund_account(bank: &Bank, pubkey: &Pubkey, lamports: u64) {
    bank.store_account(
        pubkey,
        &Account {
            lamports,
            data: vec![],
            owner: system_program::id(),
            executable: false,
            rent_epoch: Epoch::MAX,
        },
    );
}

pub(crate) fn fund_validator_identity(bank: &Bank, validator_id: &Pubkey) {
    fund_account(bank, validator_id, u64::MAX / 2);
}

pub(crate) fn funded_faucet(bank: &Bank) -> Keypair {
    let faucet = Keypair::new();
    fund_account(bank, &faucet.pubkey(), u64::MAX / 2);
    faucet
}
