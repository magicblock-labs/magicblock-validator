use std::sync::Arc;

use sleipnir_bank::bank::Bank;
use sleipnir_mutator::{
    program::{create_program_modifications, ProgramModifications},
    transactions::{
        transaction_to_clone_program, transaction_to_clone_regular_account,
    },
    AccountModification,
};
use sleipnir_processor::execute_transaction::execute_legacy_transaction;
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{
    account::Account, pubkey::Pubkey, signature::Signature,
    transaction::Transaction,
};

use crate::{AccountDumper, AccountDumperError, AccountDumperResult};

pub struct AccountDumperBank {
    bank: Arc<Bank>,
    transaction_status_sender: Option<TransactionStatusSender>,
}

impl AccountDumperBank {
    pub fn new(
        bank: Arc<Bank>,
        transaction_status_sender: Option<TransactionStatusSender>,
    ) -> Self {
        Self {
            bank,
            transaction_status_sender,
        }
    }

    fn execute_transaction(
        &self,
        transaction: Transaction,
    ) -> AccountDumperResult<Signature> {
        execute_legacy_transaction(
            transaction,
            &self.bank,
            self.transaction_status_sender.as_ref(),
        )
        .map_err(AccountDumperError::TransactionError)
    }
}

impl AccountDumper for AccountDumperBank {
    fn dump_wallet_account(
        &self,
        pubkey: &Pubkey,
        lamports: u64,
        owner: &Pubkey,
    ) -> AccountDumperResult<Signature> {
        let account = Account {
            lamports,
            owner: *owner,
            ..Default::default()
        };
        let transaction = transaction_to_clone_regular_account(
            pubkey,
            &account,
            None,
            self.bank.last_blockhash(),
        );
        self.execute_transaction(transaction)
    }

    fn dump_undelegated_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> AccountDumperResult<Signature> {
        let transaction = transaction_to_clone_regular_account(
            pubkey,
            account,
            None,
            self.bank.last_blockhash(),
        );
        self.execute_transaction(transaction)
    }

    fn dump_delegated_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        owner: &Pubkey,
    ) -> AccountDumperResult<Signature> {
        let overrides = Some(AccountModification {
            pubkey: *pubkey,
            owner: Some(*owner),
            ..Default::default()
        });
        let transaction = transaction_to_clone_regular_account(
            pubkey,
            account,
            overrides,
            self.bank.last_blockhash(),
        );
        self.execute_transaction(transaction)
    }

    fn dump_program_accounts(
        &self,
        program_id_pubkey: &Pubkey,
        program_id_account: &Account,
        program_data_pubkey: &Pubkey,
        program_data_account: &Account,
        program_idl: Option<(Pubkey, Account)>,
    ) -> AccountDumperResult<Signature> {
        let ProgramModifications {
            program_id_modification,
            program_data_modification,
            program_buffer_modification,
        } = create_program_modifications(
            program_id_pubkey,
            program_id_account,
            program_data_pubkey,
            program_data_account,
            self.bank.slot(),
        )
        .map_err(AccountDumperError::MutatorModificationError)?;
        let program_idl_modification =
            program_idl.map(|(program_idl_pubkey, program_idl_account)| {
                AccountModification::from((
                    &program_idl_pubkey,
                    &program_idl_account,
                ))
            });
        let needs_upgrade = self.bank.has_account(program_id_pubkey);
        let transaction = transaction_to_clone_program(
            needs_upgrade,
            program_id_modification,
            program_data_modification,
            program_buffer_modification,
            program_idl_modification,
            self.bank.last_blockhash(),
        );
        self.execute_transaction(transaction)
    }
}
