use std::collections::HashSet;

use solana_pubkey::Pubkey;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::Instruction,
    message::{
        v0::Message, AddressLookupTableAccount, CompileError, VersionedMessage,
    },
    signature::Keypair,
    signer::{Signer},
    transaction::VersionedTransaction,
};

use crate::tasks::{task_strategist::TaskStrategistResult, tasks::L1Task};

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
        compute_unit_price: u64,
        lookup_tables: &[AddressLookupTableAccount],
    ) -> TaskStrategistResult<VersionedTransaction> {
        let budget_instructions = Self::budget_instructions(
            Self::tasks_compute_units(&tasks),
            compute_unit_price,
        );
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
    ) -> TaskStrategistResult<VersionedTransaction> {
        // This is needed because VersionedMessage::serialize uses unwrap() ¯\_(ツ)_/¯
        instructions
            .iter()
            .map(|el| {
                if el.data.len() > u16::MAX as usize {
                    Err(crate::tasks::task_strategist::Error::FailedToFitError)
                } else {
                    Ok(())
                }
            })
            .collect::<TaskStrategistResult<_>>()?;

        let message = match Message::try_compile(
            &authority.pubkey(),
            &[budget_instructions, instructions].concat(),
            &lookup_tables,
            Hash::new_unique(),
        ) {
            Ok(message) => Ok(message),
            Err(CompileError::AccountIndexOverflow)
            | Err(CompileError::AddressTableLookupIndexOverflow) => {
                Err(crate::tasks::task_strategist::Error::FailedToFitError)
            }
            Err(CompileError::UnknownInstructionKey(pubkey)) => {
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
        )
        .expect("Signing transaction has to be non-failing");

        Ok(tx)
    }

    pub fn tasks_compute_units(tasks: &[impl AsRef<dyn L1Task>]) -> u32 {
        tasks.iter().map(|task| task.as_ref().compute_units()).sum()
    }

    pub fn budget_instructions(
        compute_units: u32,
        compute_unit_price: u64,
    ) -> [Instruction; 2] {
        let compute_budget_ix =
            ComputeBudgetInstruction::set_compute_unit_limit(compute_units);
        let compute_unit_price_ix =
            ComputeBudgetInstruction::set_compute_unit_price(
                compute_unit_price,
            );
        [compute_budget_ix, compute_unit_price_ix]
    }
}
