use std::sync::Arc;

use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::{
    blocks::BlockHash,
    transactions::{
        ProcessableTransaction, TransactionProcessingMode, TxnToProcessTx,
    },
};
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
    accountsdb: Arc<AccountsDb>,
    execution_tx: TxnToProcessTx,
}

impl AccountDumperBank {
    pub fn new(
        accountsdb: Arc<AccountsDb>,
        execution_tx: TxnToProcessTx,
    ) -> Self {
        Self {
            accountsdb,
            execution_tx,
        }
    }

    fn execute_transaction(
        &self,
        transaction: Transaction,
    ) -> AccountDumperResult<Signature> {
        let transaction = SanitizedTransaction::try_from_legacy_transaction(
            transaction,
            &Default::default(),
        )
        .map_err(AccountDumperError::TransactionError)?;
        let signature = *transaction.signature();
        let txn = ProcessableTransaction {
            transaction,
            mode: TransactionProcessingMode::Execution(None),
        };
        // NOTE: this is an example code, this is not supposed to be approved
        // instead proper async handling should be implemented in the new cloning pipeline
        let _ = self.execution_tx.try_send(txn);
        Ok(signature)
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
            // NOTE: BOGUS blockhash, this code will be replaced before merge to master
            // this is only to present make the compiler happy
            BlockHash::new_unique(),
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
            // NOTE: BOGUS blockhash, this code will be replaced before merge to master
            // this is only to present make the compiler happy
            BlockHash::new_unique(),
        );
        let result = self.execute_transaction(transaction)?;
        if let Some(mut acc) = self.accountsdb.get_account(pubkey) {
            acc.set_delegated(false);
            self.accountsdb.insert_account(pubkey, &acc);
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
            // NOTE: BOGUS blockhash, this code will be replaced before merge to master
            // this is only to present make the compiler happy
            BlockHash::new_unique(),
        );
        let result = self.execute_transaction(transaction)?;
        if let Some(mut acc) = self.accountsdb.get_account(pubkey) {
            acc.set_delegated(true);
            self.accountsdb.insert_account(pubkey, &acc);
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
            self.accountsdb.slot(),
        )
        .map_err(AccountDumperError::MutatorModificationError)?;
        let program_idl_modification =
            program_idl.map(|(program_idl_pubkey, program_idl_account)| {
                AccountModification::from((
                    &program_idl_pubkey,
                    &program_idl_account,
                ))
            });
        let needs_upgrade = self.accountsdb.contains_account(program_id_pubkey);
        let transaction = transaction_to_clone_program(
            needs_upgrade,
            program_id_modification,
            program_data_modification,
            program_buffer_modification,
            program_idl_modification,
            // NOTE: BOGUS blockhash, this code will be replaced before merge to master
            // this is only to present make the compiler happy
            BlockHash::new_unique(),
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
        let slot = self.accountsdb.slot();

        // we can use the whole data field of program, as it only contains the executable bytecode
        let program_data_modification = create_program_data_modification(
            &programdata_address,
            &program_account.data,
            slot,
        );

        let mut program_id_modification =
            AccountModification::from((program_pubkey, program_account));
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

        let needs_upgrade = self.accountsdb.contains_account(program_pubkey);

        let transaction = transaction_to_clone_program(
            needs_upgrade,
            program_id_modification,
            program_data_modification,
            program_buffer_modification,
            None,
            // NOTE: BOGUS blockhash, this code will be replaced before merge to master
            // this is only to present make the compiler happy
            BlockHash::new_unique(),
        );
        self.execute_transaction(transaction)
    }
}
