use std::collections::HashSet;

use solana_pubkey::Pubkey;
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    message::{v0::Message, AddressLookupTableAccount, VersionedMessage},
    signature::Keypair,
    signer::Signer,
    transaction::VersionedTransaction,
};

use crate::transaction_preperator::{
    budget_calculator::ComputeBudgetV1, tasks::L1Task,
};

/// Returns [`Vec<AddressLookupTableAccount>`] where all TX accounts stored in ALT
pub fn estimate_lookup_tables_for_tx(
    transaction: &VersionedTransaction,
) -> Vec<AddressLookupTableAccount> {
    transaction
        .message
        .static_account_keys()
        .chunks(256)
        .map(|addresses| AddressLookupTableAccount {
            key: Pubkey::new_unique(),
            addresses: addresses.to_vec(),
        })
        .collect()
}

pub struct TransactionUtils;
impl TransactionUtils {
    pub fn dummy_lookup_table(
        pubkeys: &[Pubkey],
    ) -> Vec<AddressLookupTableAccount> {
        pubkeys
            .chunks(256)
            .map(|addresses| AddressLookupTableAccount {
                key: Pubkey::new_unique(),
                addresses: addresses.to_vec(),
            })
            .collect()
    }

    pub fn unique_involved_pubkeys(
        tasks: &[Box<dyn L1Task>],
        validator: &Pubkey,
        budget_instructions: &[Instruction],
    ) -> Vec<Pubkey> {
        // Collect all unique pubkeys from tasks and budget instructions
        let mut all_pubkeys: HashSet<Pubkey> = tasks
            .iter()
            .flat_map(|task| task.involved_accounts(validator))
            .collect();

        all_pubkeys.extend(
            budget_instructions
                .iter()
                .flat_map(|ix| ix.accounts.iter().map(|meta| meta.pubkey)),
        );

        all_pubkeys.into_iter().collect::<Vec<_>>()
    }

    pub fn tasks_instructions(
        validator: &Pubkey,
        tasks: &[Box<dyn L1Task>],
    ) -> Vec<Instruction> {
        tasks
            .iter()
            .map(|task| task.instruction(validator))
            .collect()
    }

    pub fn assemble_tasks_tx(
        authority: &Keypair,
        tasks: &[Box<dyn L1Task>],
        lookup_tables: &[AddressLookupTableAccount],
    ) -> VersionedTransaction {
        let budget_instructions =
            Self::budget_instructions(&Self::tasks_budgets(&tasks));
        let ixs = Self::tasks_instructions(&authority.pubkey(), &tasks);
        Self::assemble_tx_raw(
            authority,
            &ixs,
            &budget_instructions,
            lookup_tables,
        )
    }

    pub fn assemble_tx_raw(
        authority: &Keypair,
        instructions: &[Instruction],
        budget_instructions: &[Instruction],
        lookup_tables: &[AddressLookupTableAccount],
    ) -> VersionedTransaction {
        let message = Message::try_compile(
            &authority.pubkey(),
            &[budget_instructions, instructions].concat(),
            &lookup_tables,
            Hash::new_unique(),
        )
        .unwrap(); // TODO(edwin): unwrap
        let tx = VersionedTransaction::try_new(
            VersionedMessage::V0(message),
            &[authority],
        )
        .unwrap();
        tx
    }

    pub fn tasks_budgets(
        tasks: &[impl AsRef<dyn L1Task>],
    ) -> Vec<ComputeBudgetV1> {
        tasks
            .iter()
            .map(|task| task.as_ref().budget())
            .collect::<Vec<_>>()
    }

    pub fn budget_instructions(
        budgets: &[ComputeBudgetV1],
    ) -> [Instruction; 2] {
        todo!()
    }
}
