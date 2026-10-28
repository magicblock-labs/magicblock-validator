use std::collections::HashSet;

use delegation_program_interface::legacy::{AccountSizeClass, DLP_PROGRAM_DATA_SIZE_CLASS};
use mbv_core::intent::{BaseAction, types::CommittedAccount};
use solana_account::Account;
use solana_compute_budget::compute_budget_limits::ComputeBudgetLimits;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{AddressLookupTableAccount, CompileError, VersionedMessage, v0::Message, v1};
use solana_pubkey::{Pubkey, pubkey};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

use crate::shared::tasks::{
    BaseActionTask, BaseActionTaskV1, BaseActionTaskV2, Task,
    commit_delivery::CommitDelivery,
    commit_finalize_task::CommitFinalizeTask,
    task_strategist::{TaskStrategistError, TaskStrategistResult},
};

// Accounts larger than COMMIT_STATE_SIZE_THRESHOLD use CommitDiff to
// reduce instruction size. Below this threshold, the commit is sent
// as CommitState. The value (256) is chosen because it is sufficient
// for small accounts, which typically could hold up to 8 u32 fields or
// 4 u64 fields. These integers are expected to be on the hot path
// and updated continuously.
pub(crate) const COMMIT_STATE_SIZE_THRESHOLD: usize = 256;

/// Builds a [`Task`] for each `action`, used by both
/// [`crate::execution::task_builder::TaskBuilder`] (real task construction)
/// and [`crate::is_intent_within_limits`] (size
/// estimation) -- actions carry no unknowns that need fetching, so both
/// build them identically.
pub(crate) fn create_action_tasks(actions: &[BaseAction]) -> impl Iterator<Item = Task> + '_ {
    actions.iter().map(|action| {
        let task = match action.source_program {
            Some(source_program) => BaseActionTask::V2(BaseActionTaskV2 {
                action: action.clone(),
                source_program,
            }),
            None => BaseActionTask::V1(BaseActionTaskV1 { action: action.clone() }),
        };
        task.into()
    })
}

fn commit_delivery(account: &CommittedAccount, base_account: Option<Account>) -> CommitDelivery {
    let base_account = if account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD {
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

/// Builds a combined commit and finalization task for `account`.
pub(crate) fn create_commit_finalize_task(
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

pub(crate) struct TransactionUtils;
impl TransactionUtils {
    pub(crate) const COMPUTE_BUDGET_INSTRUCTION_COUNT: u8 = 2;
    const UNIQUENESS_NOOP_PROGRAM_DATA_SIZE_BUDGET: u32 = 42 * 1024;
    const UNIQUENESS_NOOP_COMPUTE_UNITS: u32 = 1_000;
    const UNIQUENESS_NOOP_PROGRAM_ID: Pubkey =
        pubkey!("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV");

    pub(crate) fn dummy_lookup_table(pubkeys: &[Pubkey]) -> Vec<AddressLookupTableAccount> {
        pubkeys
            .chunks(256)
            .map(|addresses| AddressLookupTableAccount {
                key: Pubkey::new_unique(),
                addresses: addresses.to_vec(),
            })
            .collect()
    }

    pub(crate) fn unique_involved_pubkeys(
        tasks: &[Task],
        validator: &Pubkey,
        budget_instructions: &[Instruction],
    ) -> Vec<Pubkey> {
        // Collect all unique pubkeys from tasks and budget instructions
        let mut all_pubkeys: HashSet<Pubkey> =
            tasks.iter().flat_map(|task| task.involved_accounts(validator)).collect();

        all_pubkeys.extend(
            budget_instructions
                .iter()
                .flat_map(|ix| ix.accounts.iter().map(|meta| meta.pubkey)),
        );

        all_pubkeys.into_iter().collect::<Vec<_>>()
    }

    pub(crate) fn tasks_instructions(validator: &Pubkey, tasks: &[Task]) -> Vec<Instruction> {
        tasks.iter().map(|task| task.instruction(validator)).collect()
    }

    pub(crate) fn assemble_tasks_tx_with_uniqueness_nonce(
        authority: &Keypair,
        tasks: &[Task],
        compute_unit_price: u64,
        lookup_tables: &[AddressLookupTableAccount],
        uniqueness_nonce: Option<u64>,
    ) -> TaskStrategistResult<VersionedTransaction> {
        let budget_instructions = Self::budget_instructions(
            Self::tasks_compute_units(tasks, uniqueness_nonce),
            compute_unit_price,
            Self::tasks_accounts_size_budget_with_uniqueness_nonce(tasks, uniqueness_nonce),
        );
        let mut ixs = Self::tasks_instructions(&authority.pubkey(), tasks);
        if let Some(nonce) = uniqueness_nonce {
            ixs.push(Self::uniqueness_noop_instruction(nonce));
        }
        Self::assemble_tx_raw(authority, &ixs, &budget_instructions, lookup_tables)
    }

    /// Builds a native v1 transaction without compute-budget instructions or lookup tables.
    pub(crate) fn assemble_tasks_v1_tx_with_uniqueness_nonce(
        authority: &Keypair,
        tasks: &[Task],
        compute_unit_price: u64,
        uniqueness_nonce: Option<u64>,
    ) -> TaskStrategistResult<VersionedTransaction> {
        let config = Self::v1_config(
            Self::tasks_compute_units(tasks, uniqueness_nonce),
            compute_unit_price,
        );
        let mut instructions = Self::tasks_instructions(&authority.pubkey(), tasks);
        if let Some(nonce) = uniqueness_nonce {
            instructions.push(Self::uniqueness_noop_instruction(nonce));
        }
        let message = v1::Message::try_compile_with_config(
            &authority.pubkey(),
            &instructions,
            Hash::new_unique(),
            config,
        )?;
        message.validate().map_err(|_| TaskStrategistError::FailedToFit)?;
        Ok(VersionedTransaction::try_new(
            VersionedMessage::V1(message),
            &[authority],
        )?)
    }

    pub(crate) fn assemble_tx_raw(
        authority: &Keypair,
        instructions: &[Instruction],
        budget_instructions: &[Instruction],
        lookup_tables: &[AddressLookupTableAccount],
    ) -> TaskStrategistResult<VersionedTransaction> {
        // This is needed because VersionedMessage::serialize uses unwrap() ¯\_(ツ)_/¯
        instructions.iter().try_for_each(|el| {
            if el.data.len() > u16::MAX as usize {
                Err(TaskStrategistError::FailedToFit)
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
                Err(TaskStrategistError::FailedToFit)
            }
            Err(err @ CompileError::UnknownInstructionKey(_)) => Err(err.into()),
        }?;

        // SignerError is critical
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[authority])?;

        Ok(tx)
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

    pub(crate) fn tasks_compute_units(tasks: &[Task], uniqueness_nonce: Option<u64>) -> u32 {
        // Empty recovery stages can still execute the uniqueness no-op.
        let overhead = uniqueness_nonce.map_or(0, |_| Self::UNIQUENESS_NOOP_COMPUTE_UNITS);
        tasks.iter().fold(overhead, |sum, task| {
            sum.saturating_add(task.compute_units())
        })
    }

    pub(crate) fn tasks_accounts_size_budget(tasks: &[Task]) -> u32 {
        if tasks.is_empty() {
            return 0;
        }

        let total_budget: u32 = tasks.iter().map(|task| task.accounts_size_budget()).sum();

        // All tasks target DLP, so count its program data only once.
        let dlp_program_budget = DLP_PROGRAM_DATA_SIZE_CLASS.size_budget();
        let deduction = (tasks.len() as u32).saturating_sub(1).saturating_mul(dlp_program_budget);
        // The API's 350 KiB estimate is smaller than the deployed DLP.
        // Reserve at least 1 MiB for its program data, counted only once.
        let program_headroom =
            AccountSizeClass::Huge.size_budget().saturating_sub(dlp_program_budget);
        total_budget.saturating_sub(deduction).saturating_add(program_headroom)
    }

    fn tasks_accounts_size_budget_with_uniqueness_nonce(
        tasks: &[Task],
        uniqueness_nonce: Option<u64>,
    ) -> u32 {
        let budget = Self::tasks_accounts_size_budget(tasks);
        if uniqueness_nonce.is_some() {
            budget.saturating_add(Self::UNIQUENESS_NOOP_PROGRAM_DATA_SIZE_BUDGET)
        } else {
            budget
        }
    }

    fn v1_config(compute_units: u32, compute_unit_price: u64) -> v1::TransactionConfig {
        let limits = ComputeBudgetLimits {
            compute_unit_limit: compute_units,
            compute_unit_price,
            ..ComputeBudgetLimits::default()
        };
        // Task estimates don't know actual base-chain account or program sizes.
        // Use the runtime default explicitly; omitting the v1 limit means zero.
        // TODO: Use a better estimate of loaded account and program data, with
        // headroom, instead of the maximum to reduce scheduler cost.
        v1::TransactionConfig::empty()
            .with_priority_fee(limits.get_prioritization_fee())
            .with_compute_unit_limit(compute_units)
            .with_loaded_accounts_data_size_limit(limits.loaded_accounts_bytes.get())
    }

    pub(crate) fn budget_instructions(
        compute_units: u32,
        compute_unit_price: u64,
        _accounts_size_budget: u32,
    ) -> [Instruction; 2] {
        [
            ComputeBudgetInstruction::set_compute_unit_limit(compute_units),
            ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price),
        ]
    }
}
