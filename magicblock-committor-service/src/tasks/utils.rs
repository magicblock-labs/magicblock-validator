use std::collections::HashSet;

use dlp_api::DLP_PROGRAM_DATA_SIZE_CLASS;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{
    v0::Message, AddressLookupTableAccount, CompileError, VersionedMessage,
};
use solana_pubkey::{pubkey, Pubkey};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

use crate::{
    tasks::{
        task_strategist::{TaskStrategistError, TaskStrategistResult},
        BaseTask, BaseTaskImpl,
    },
    transactions::{serialize_and_encode_base64, MAX_ENCODED_TRANSACTION_SIZE},
};

pub struct TransactionUtils;
impl TransactionUtils {
    const STANDALONE_ACTION_NOOP_PROGRAM_ID: Pubkey =
        pubkey!("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV");

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
        tasks: &[BaseTaskImpl],
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
        tasks: &[BaseTaskImpl],
    ) -> Vec<Instruction> {
        tasks
            .iter()
            .map(|task| task.instruction(validator))
            .collect()
    }

    pub fn assemble_tasks_tx(
        authority: &Keypair,
        tasks: &[BaseTaskImpl],
        compute_unit_price: u64,
        lookup_tables: &[AddressLookupTableAccount],
    ) -> TaskStrategistResult<VersionedTransaction> {
        Self::assemble_tasks_tx_with_standalone_action_nonce(
            authority,
            tasks,
            compute_unit_price,
            lookup_tables,
            None,
        )
    }

    pub fn assemble_tasks_tx_with_standalone_action_nonce(
        authority: &Keypair,
        tasks: &[BaseTaskImpl],
        compute_unit_price: u64,
        lookup_tables: &[AddressLookupTableAccount],
        standalone_action_nonce: Option<u64>,
    ) -> TaskStrategistResult<VersionedTransaction> {
        let budget_instructions = Self::budget_instructions(
            Self::tasks_compute_units(tasks),
            compute_unit_price,
            Self::tasks_accounts_size_budget(tasks),
        );
        let mut ixs = Self::tasks_instructions(&authority.pubkey(), tasks);
        if let Some(nonce) = standalone_action_nonce {
            ixs.push(Self::standalone_action_noop_instruction(nonce));
        }
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

        if serialize_and_encode_base64(&tx).len() > MAX_ENCODED_TRANSACTION_SIZE
        {
            return Err(TaskStrategistError::FailedToFitError);
        }

        Ok(tx)
    }

    fn standalone_action_noop_instruction(id: u64) -> Instruction {
        // TODO(GabrielePicco): replace this temporary transaction-level
        // uniqueness padding with protocol-level standalone action nonces.
        Instruction {
            program_id: Self::STANDALONE_ACTION_NOOP_PROGRAM_ID,
            accounts: vec![],
            data: id.to_le_bytes().to_vec(),
        }
    }

    pub fn tasks_compute_units(tasks: &[BaseTaskImpl]) -> u32 {
        tasks.iter().map(|task| task.compute_units()).sum()
    }

    pub fn tasks_accounts_size_budget(tasks: &[BaseTaskImpl]) -> u32 {
        if tasks.is_empty() {
            return 0;
        }

        let total_budget: u32 =
            tasks.iter().map(|task| task.accounts_size_budget()).sum();

        let dlp_task_count: u32 = tasks
            .iter()
            .filter(|task| task.program_id() == dlp_api::id())
            .count() as u32;

        if dlp_task_count > 0 {
            let dlp_program_budget = DLP_PROGRAM_DATA_SIZE_CLASS.size_budget();
            let deduction = dlp_task_count
                .saturating_sub(1)
                .saturating_mul(dlp_program_budget);
            total_budget.saturating_sub(deduction)
        } else {
            total_budget
        }
    }

    pub fn budget_instructions(
        compute_units: u32,
        compute_unit_price: u64,
        _accounts_size_budget: u32,
    ) -> [Instruction; 2] {
        [
            ComputeBudgetInstruction::set_compute_unit_limit(compute_units),
            ComputeBudgetInstruction::set_compute_unit_price(
                compute_unit_price,
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_strategist::TaskStrategistError;

    #[test]
    fn assemble_tx_raw_rejects_oversized_transaction() {
        let authority = Keypair::new();
        let ix = Instruction {
            program_id: Pubkey::new_unique(),
            accounts: vec![],
            data: vec![0; MAX_ENCODED_TRANSACTION_SIZE],
        };

        let result =
            TransactionUtils::assemble_tx_raw(&authority, &[ix], &[], &[]);

        assert!(matches!(result, Err(TaskStrategistError::FailedToFitError)));
    }
}
