use std::sync::Arc;

use sleipnir_bank::bank::Bank;
use sleipnir_mutator::{
    program::{create_program_modifications, ProgramModifications},
    transactions::{
        transaction_to_clone_regular_account, transactions_to_clone_program,
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

    fn execute_transactions(
        &self,
        transactions: Vec<Transaction>,
    ) -> AccountDumperResult<Vec<Signature>> {
        transactions
            .into_iter()
            .map(|transaction| self.execute_transaction(transaction))
            .collect()
    }
}

impl AccountDumper for AccountDumperBank {
    fn dump_system_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        lamports: Option<u64>,
    ) -> AccountDumperResult<Signature> {
        let overrides = lamports.map(|lamports| AccountModification {
            pubkey: *pubkey,
            lamports: Some(lamports),
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

    fn dump_pda_account(
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

    fn dump_program(
        &self,
        program_id_pubkey: &Pubkey,
        program_id_account: &Account,
        program_data_pubkey: &Pubkey,
        program_data_account: &Account,
        program_idl: Option<(&Pubkey, &Account)>,
    ) -> AccountDumperResult<Vec<Signature>> {
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
            program_idl.map(AccountModification::from);
        let needs_upgrade = self.bank.get_account(program_id_pubkey).is_some();
        let transactions = transactions_to_clone_program(
            needs_upgrade,
            program_id_modification,
            program_data_modification,
            program_buffer_modification,
            program_idl_modification,
            self.bank.last_blockhash(),
        );
        self.execute_transactions(transactions)
    }
}
