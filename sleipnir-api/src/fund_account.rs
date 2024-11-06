use sleipnir_bank::bank::Bank;
use sleipnir_core::magic_program;
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

pub(crate) fn fund_account_with_data(
    bank: &Bank,
    pubkey: &Pubkey,
    lamports: u64,
    data: Vec<u8>,
) {
    bank.store_account(
        pubkey,
        &Account {
            lamports,
            data,
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
    // TODO @@@ we need to store the faucet keypair with the ledger such that we
    // can properly replay airdrops
    let faucet =
        <Keypair as solana_sdk::signer::SeedDerivable>::from_seed(&[45; 32])
            .unwrap();
    fund_account(bank, &faucet.pubkey(), u64::MAX / 2);
    faucet
}

pub(crate) fn fund_magic_context(bank: &Bank) {
    fund_account_with_data(
        bank,
        &magic_program::MAGIC_CONTEXT_PUBKEY,
        u64::MAX,
        vec![0; magic_program::MAGIC_CONTEXT_SIZE],
    );
}
