use std::collections::HashSet;

use dlp_api::DLP_PROGRAM_DATA_SIZE_CLASS;
use magicblock_core::intent::{types::CommittedAccount, BaseAction};
use solana_account::Account;
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
        commit_finalize_task::CommitFinalizeTask,
        commit_task::{CommitDelivery, CommitTask},
        task_strategist::TaskStrategistResult,
        BaseActionTask, BaseActionTaskV1, BaseActionTaskV2, BaseTask,
        BaseTaskImpl,
    },
    transactions::v1,
};

// Accounts larger than COMMIT_STATE_SIZE_THRESHOLD use CommitDiff to
// reduce instruction size. Below this threshold, the commit is sent
// as CommitState. The value (256) is chosen because it is sufficient
// for small accounts, which typically could hold up to 8 u32 fields or
// 4 u64 fields. These integers are expected to be on the hot path
// and updated continuously.
pub const COMMIT_STATE_SIZE_THRESHOLD: usize = 256;

/// Builds a [`BaseTaskImpl`] for each `action`, used by both
/// [`crate::tasks::task_builder::TaskBuilderImpl`] (real task construction)
/// and [`crate::tasks::intent_size_validator::IntentSizeValidator`] (size
/// estimation) -- actions carry no unknowns that need fetching, so both
/// build them identically.
pub fn create_action_tasks(
    actions: &[BaseAction],
) -> impl Iterator<Item = BaseTaskImpl> + '_ {
    actions.iter().map(|action| {
        let task = match action.source_program {
            Some(source_program) => BaseActionTask::V2(BaseActionTaskV2 {
                action: action.clone(),
                source_program,
            }),
            None => BaseActionTask::V1(BaseActionTaskV1 {
                action: action.clone(),
            }),
        };
        task.into()
    })
}

/// Decides how a commit's data should be delivered based on account size:
/// accounts larger than `COMMIT_STATE_SIZE_THRESHOLD` diff against
/// `base_account` (when available), everything else is sent as full state.
/// Shared by [`create_commit_task`] and [`create_commit_finalize_task`] so
/// the two never drift apart.
fn commit_delivery(
    account: &CommittedAccount,
    base_account: Option<Account>,
) -> CommitDelivery {
    let base_account =
        if account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD {
            base_account
        } else {
            None
        };

    if let Some(base_account) = base_account {
        CommitDelivery::DiffInArgs { base_account }
    } else {
        CommitDelivery::StateInArgs
    }
}

/// Builds a [`CommitTask`] for `account`, used by both
/// [`crate::tasks::task_builder::TaskBuilderImpl`] (real task construction,
/// passing the real base-layer account state to diff against) and
/// [`crate::tasks::intent_size_validator::IntentSizeValidator`] (size
/// estimation, passing a stand-in base account purely to exercise this same
/// `COMMIT_STATE_SIZE_THRESHOLD` check).
pub fn create_commit_task(
    commit_id: u64,
    allow_undelegation: bool,
    account: CommittedAccount,
    base_account: Option<Account>,
) -> CommitTask {
    let delivery_details = commit_delivery(&account, base_account);

    CommitTask {
        commit_id,
        allow_undelegation,
        committed_account: account,
        delivery_details,
    }
}

/// Same as [`create_commit_task`] but for [`CommitFinalizeTask`].
pub fn create_commit_finalize_task(
    commit_id: u64,
    allow_undelegation: bool,
    account: CommittedAccount,
    base_account: Option<Account>,
) -> CommitFinalizeTask {
    let delivery_details = commit_delivery(&account, base_account);

    CommitFinalizeTask {
        commit_id,
        allow_undelegation,
        committed_account: account,
        delivery: delivery_details,
    }
}

pub struct TransactionUtils;
impl TransactionUtils {
    pub(crate) const COMPUTE_BUDGET_INSTRUCTION_COUNT: u8 = 2;
    const UNIQUENESS_NOOP_PROGRAM_ID: Pubkey =
        pubkey!("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV");
    const MICRO_LAMPORTS_PER_LAMPORT: u64 = 1_000_000;

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
        Self::assemble_tasks_tx_with_uniqueness_nonce(
            authority,
            tasks,
            compute_unit_price,
            lookup_tables,
            None,
        )
    }

    pub fn assemble_tasks_tx_with_uniqueness_nonce(
        authority: &Keypair,
        tasks: &[BaseTaskImpl],
        compute_unit_price: u64,
        lookup_tables: &[AddressLookupTableAccount],
        uniqueness_nonce: Option<u64>,
    ) -> TaskStrategistResult<VersionedTransaction> {
        let budget_instructions = Self::budget_instructions(
            Self::tasks_compute_units(tasks),
            compute_unit_price,
            Self::tasks_accounts_size_budget(tasks),
        );
        let mut ixs = Self::tasks_instructions(&authority.pubkey(), tasks);
        if let Some(nonce) = uniqueness_nonce {
            ixs.push(Self::uniqueness_noop_instruction(nonce));
        }
        Self::assemble_tx_raw(
            authority,
            &ixs,
            &budget_instructions,
            lookup_tables,
        )
    }

    pub(crate) fn assemble_tasks_v1_tx_with_uniqueness_nonce(
        authority: &Keypair,
        tasks: &[BaseTaskImpl],
        compute_unit_price: u64,
        uniqueness_nonce: Option<u64>,
    ) -> TaskStrategistResult<v1::Transaction> {
        let message = Self::assemble_tasks_v1_message_with_uniqueness_nonce(
            authority,
            tasks,
            compute_unit_price,
            uniqueness_nonce,
        )?;
        Ok(v1::Transaction::try_new(message, authority)?)
    }

    pub(crate) fn assemble_tasks_v1_message_with_uniqueness_nonce(
        authority: &Keypair,
        tasks: &[BaseTaskImpl],
        compute_unit_price: u64,
        uniqueness_nonce: Option<u64>,
    ) -> TaskStrategistResult<v1::Message> {
        let compute_units = Self::tasks_compute_units(tasks);
        let config = Self::v1_config(
            compute_units,
            compute_unit_price,
            Self::tasks_accounts_size_budget(tasks),
        );
        let mut ixs = Self::tasks_instructions(&authority.pubkey(), tasks);
        if let Some(nonce) = uniqueness_nonce {
            ixs.push(Self::uniqueness_noop_instruction(nonce));
        }
        Self::assemble_v1_message_raw(authority, &ixs, config)
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

    pub(crate) fn assemble_v1_message_raw(
        authority: &Keypair,
        instructions: &[Instruction],
        config: v1::TransactionConfig,
    ) -> TaskStrategistResult<v1::Message> {
        let message = match v1::Message::try_compile_with_config(
            &authority.pubkey(),
            instructions,
            Hash::new_unique(),
            config,
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

        message.validate().map_err(|_| {
            crate::tasks::task_strategist::TaskStrategistError::FailedToFitError
        })?;
        Ok(message)
    }

    pub(crate) fn uniqueness_noop_instruction(id: u64) -> Instruction {
        // TODO(GabrielePicco): replace this temporary transaction-level
        // uniqueness padding with protocol-level standalone action nonces.
        Instruction {
            program_id: Self::UNIQUENESS_NOOP_PROGRAM_ID,
            accounts: vec![],
            data: id.to_le_bytes().to_vec(),
        }
    }

    pub fn tasks_compute_units(tasks: &[BaseTaskImpl]) -> u32 {
        tasks
            .iter()
            .fold(0, |sum, task| sum.saturating_add(task.compute_units()))
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

    fn v1_config(
        compute_units: u32,
        compute_unit_price: u64,
        accounts_size_budget: u32,
    ) -> v1::TransactionConfig {
        v1::TransactionConfig::empty()
            .with_priority_fee(Self::priority_fee_lamports(
                compute_unit_price,
                compute_units,
            ))
            .with_compute_unit_limit(compute_units)
            .with_loaded_accounts_data_size_limit(accounts_size_budget)
    }

    fn priority_fee_lamports(
        compute_unit_price: u64,
        compute_units: u32,
    ) -> u64 {
        let fee = (compute_unit_price as u128)
            .saturating_mul(u128::from(compute_units))
            .saturating_add(
                u128::from(Self::MICRO_LAMPORTS_PER_LAMPORT).saturating_sub(1),
            )
            / u128::from(Self::MICRO_LAMPORTS_PER_LAMPORT);
        u64::try_from(fee).unwrap_or(u64::MAX)
    }

    pub fn budget_instructions(
        compute_units: u32,
        compute_unit_price: u64,
        _accounts_size_budget: u32,
    ) -> [Instruction; Self::COMPUTE_BUDGET_INSTRUCTION_COUNT as usize] {
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
    use super::TransactionUtils;

    #[test]
    fn v1_priority_fee_matches_compute_budget_rounding() {
        assert_eq!(TransactionUtils::priority_fee_lamports(0, 100), 0);
        assert_eq!(TransactionUtils::priority_fee_lamports(1, 1), 1);
        assert_eq!(
            TransactionUtils::priority_fee_lamports(1_000_000, 345),
            345
        );
        assert_eq!(
            TransactionUtils::priority_fee_lamports(u64::MAX, u32::MAX),
            u64::MAX
        );
    }
}
