use std::collections::HashSet;

use dlp::DLP_PROGRAM_DATA_SIZE_CLASS;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{
    v0::Message, AddressLookupTableAccount, CompileError, VersionedMessage,
};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

use crate::tasks::{task_strategist::TaskStrategistResult, BaseTask};

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
        tasks: &[Box<dyn BaseTask>],
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
        tasks: &[Box<dyn BaseTask>],
    ) -> Vec<Instruction> {
        tasks
            .iter()
            .map(|task| task.instruction(validator))
            .collect()
    }

    pub fn assemble_tasks_tx(
        authority: &Keypair,
        tasks: &[Box<dyn BaseTask>],
        compute_unit_price: u64,
        lookup_tables: &[AddressLookupTableAccount],
    ) -> TaskStrategistResult<VersionedTransaction> {
        let budget_instructions = Self::budget_instructions(
            Self::tasks_compute_units(tasks),
            compute_unit_price,
            Self::tasks_accounts_size_budget(tasks),
        );
        let ixs = Self::tasks_instructions(&authority.pubkey(), tasks);
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
    ) -> TaskStrategistResult<VersionedTransaction> {
        // This is needed because VersionedMessage::serialize uses unwrap() ¯\_(ツ)_/¯
        instructions.iter().try_for_each(|el| {
            if el.data.len() > u16::MAX as usize {
                Err(crate::tasks::task_strategist::TaskStrategistError::FailedToFitError)
            } else {
                Ok(())
            }
        })?;

        let message = match Message::try_compile(
            &authority.pubkey(),
            &[budget_instructions, instructions].concat(),
            lookup_tables,
            Hash::new_unique(),
        ) {
            Ok(message) => Ok(message),
            Err(CompileError::AccountIndexOverflow)
            | Err(CompileError::AddressTableLookupIndexOverflow) => {
                Err(crate::tasks::task_strategist::TaskStrategistError::FailedToFitError)
            }
            Err(CompileError::UnknownInstructionKey(pubkey)) => {
                // SAFETY: this may occur in utility AccountKeys::try_compile_instructions
                // when User's pubkeys in Instruction doesn't exist in AccountKeys.
                // This is impossible in our case since AccountKeys created on keys of our Ixs
                // that means that all keys from out ixs exist in AccountKeys
                panic!(
                    "Supplied instruction has to be valid: {}",
                    CompileError::UnknownInstructionKey(pubkey)
                );
            }
        }?;

        // SignerError is critical
        let tx = VersionedTransaction::try_new(
            VersionedMessage::V0(message),
            &[authority],
        )?;

        Ok(tx)
    }

    pub fn tasks_compute_units(tasks: &[impl AsRef<dyn BaseTask>]) -> u32 {
        tasks.iter().map(|task| task.as_ref().compute_units()).sum()
    }

    pub fn tasks_accounts_size_budget(
        tasks: &[impl AsRef<dyn BaseTask>],
    ) -> u32 {
        if tasks.is_empty() {
            return 0;
        }

        let total_budget: u32 = tasks
            .iter()
            .map(|task| task.as_ref().accounts_size_budget())
            .sum();

        let dlp_task_count: u32 = tasks
            .iter()
            .filter(|task| task.as_ref().program_id() == dlp::id())
            .count() as u32;

        if dlp_task_count > 0 {
            let dlp_program_budget =
                DLP_PROGRAM_DATA_SIZE_CLASS.size_budget();
            let deduction = dlp_task_count
                .saturating_sub(1)
                .saturating_mul(dlp_program_budget);
            total_budget.saturating_sub(
                deduction,
            )
        } else {
            total_budget
        }
    }

    pub fn budget_instructions(
        compute_units: u32,
        compute_unit_price: u64,
        accounts_size_budget: u32,
    ) -> [Instruction; 3] {
        [
            ComputeBudgetInstruction::set_compute_unit_limit(compute_units),
            ComputeBudgetInstruction::set_compute_unit_price(
                compute_unit_price,
            ),
            ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                accounts_size_budget,
            ),
        ]
    }
}
