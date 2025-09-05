use std::sync::Arc;

use magicblock_bank::bank::Bank;
use magicblock_mutator::{
    program::{
        create_program_buffer_modification, create_program_data_modification,
        create_program_modifications, ProgramModifications,
    },
    transactions::{
        transaction_to_clone_program, transaction_to_clone_regular_account,
    },
    AccountModification,
};
use magicblock_processor::execute_transaction::execute_sanitized_transaction;
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::{
    account::Account,
    bpf_loader_upgradeable::{
        self, get_program_data_address, UpgradeableLoaderState,
    },
    pubkey::Pubkey,
    signature::Signature,
    transaction::{SanitizedTransaction, Transaction},
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
        let sanitized_tx = SanitizedTransaction::try_from_legacy_transaction(
            transaction,
            &Default::default(),
        )
        .map_err(AccountDumperError::TransactionError)?;
        execute_sanitized_transaction(
            sanitized_tx,
            &self.bank,
            self.transaction_status_sender.as_ref(),
        )
        .map_err(AccountDumperError::TransactionError)
    }
}

impl AccountDumper for AccountDumperBank {
    fn dump_feepayer_account(
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
        let result = self.execute_transaction(transaction)?;
        if let Some(mut acc) = self.bank.get_account(pubkey) {
            acc.set_delegated(false);
            self.bank.store_account(*pubkey, acc);
        }
        Ok(result)
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
        let result = self.execute_transaction(transaction)?;
        if let Some(mut acc) = self.bank.get_account(pubkey) {
            acc.set_delegated(true);
            self.bank.store_account(*pubkey, acc);
        }
        Ok(result)
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
                AccountModification {
                    pubkey: program_idl_pubkey,
                    lamports: Some(program_idl_account.lamports),
                    owner: Some(program_idl_account.owner),
                    executable: Some(program_idl_account.executable),
                    data: Some(program_idl_account.data.clone()),
                    rent_epoch: Some(program_idl_account.rent_epoch),
                }
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

    fn dump_program_account_with_old_bpf(
        &self,
        program_pubkey: &Pubkey,
        program_account: &Account,
    ) -> AccountDumperResult<Signature> {
        // derive program data account address, as expected by upgradeable BPF loader
        let programdata_address = get_program_data_address(program_pubkey);
        let slot = self.bank.slot();

        // we can use the whole data field of program, as it only contains the executable bytecode
        let program_data_modification = create_program_data_modification(
            &programdata_address,
            &program_account.data,
            slot,
        );

        let mut program_id_modification = AccountModification {
            pubkey: *program_pubkey,
            lamports: Some(program_account.lamports),
            owner: Some(program_account.owner),
            executable: Some(program_account.executable),
            data: Some(program_account.data.clone()),
            rent_epoch: Some(program_account.rent_epoch),
        };
        // point program account to the derived program data account address
        let program_id_state =
            bincode::serialize(&UpgradeableLoaderState::Program {
                programdata_address,
            })
            .expect("infallible serialization of UpgradeableLoaderState ");
        program_id_modification.executable.replace(true);
        program_id_modification.data.replace(program_id_state);

        // substitute the owner of the program with upgradable BPF loader
        program_id_modification
            .owner
            .replace(bpf_loader_upgradeable::ID);

        let program_buffer_modification =
            create_program_buffer_modification(&program_account.data);

        let needs_upgrade = self.bank.has_account(program_pubkey);

        let transaction = transaction_to_clone_program(
            needs_upgrade,
            program_id_modification,
            program_data_modification,
            program_buffer_modification,
            None,
            self.bank.last_blockhash(),
        );
        self.execute_transaction(transaction)
    }
}
