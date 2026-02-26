use dlp::{
    args::CallHandlerArgs,
    instruction_builder::{
        call_handler_size_budget, call_handler_v2_size_budget,
        finalize_size_budget, undelegate_size_budget,
    },
    AccountSizeClass,
};
use magicblock_committor_program::{
    instruction_builder::{
        close_buffer::{create_close_ix, CreateCloseIxArgs},
        init_buffer::{create_init_ix, CreateInitIxArgs},
        realloc_buffer::{
            create_realloc_buffer_ixs, CreateReallocBufferIxArgs,
        },
        write_buffer::{create_write_ix, CreateWriteIxArgs},
    },
    pdas, ChangesetChunks, Chunks,
};
use magicblock_metrics::metrics::LabelValue;
use magicblock_program::magic_scheduled_base_intent::{
    BaseAction, BaseActionCallback, CommittedAccount,
};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

pub mod commit_task;
pub mod task_builder;
pub mod task_strategist;
pub mod utils;

pub use task_builder::TaskBuilderImpl;

use crate::tasks::commit_task::CommitTask;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskType {
    Commit,
    Finalize,
    Undelegate,
    Action,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TaskStrategy {
    Args,
    Buffer,
}

#[derive(Clone, Debug)]
pub enum BaseTaskImpl {
    Commit(CommitTask),
    Finalize(FinalizeTask),
    Undelegate(UndelegateTask),
    BaseAction(BaseActionTask),
}

impl BaseTask for BaseTaskImpl {
    fn program_id(&self) -> Pubkey {
        dlp::id()
    }

    fn instruction(&self, validator: &Pubkey) -> Instruction {
        match self {
            Self::Commit(value) => value.instruction(validator),
            Self::Finalize(value) => value.instruction(validator),
            Self::Undelegate(value) => value.instruction(validator),
            Self::BaseAction(value) => value.instruction(validator),
        }
    }

    fn try_optimize_tx_size(&mut self) -> bool {
        match self {
            Self::Commit(value) => value.try_optimize_tx_size(),
            _ => false,
        }
    }

    fn compute_units(&self) -> u32 {
        match self {
            Self::Commit(value) => value.compute_units(),
            Self::BaseAction(value) => value.compute_units(),
            Self::Finalize(_) => 70_000,
            Self::Undelegate(_) => 70_000,
        }
    }

    fn accounts_size_budget(&self) -> u32 {
        match self {
            Self::Commit(value) => value.accounts_size_budget(),
            Self::BaseAction(value) => value.accounts_size_budget(),
            Self::Finalize(_) => finalize_size_budget(AccountSizeClass::Huge),
            Self::Undelegate(_) => {
                undelegate_size_budget(AccountSizeClass::Huge)
            }
        }
    }
}

impl BaseTaskImpl {
    pub fn strategy(&self) -> TaskStrategy {
        match self {
            Self::Commit(task) if task.is_buffer() => TaskStrategy::Buffer,
            _ => TaskStrategy::Args,
        }
    }
}

impl LabelValue for BaseTaskImpl {
    fn value(&self) -> &str {
        match self {
            Self::Commit(task) => {
                if task.is_buffer() {
                    "buffer_commit"
                } else {
                    "args_commit"
                }
            }
            Self::Finalize(_) => "args_finalize",
            Self::Undelegate(_) => "args_undelegate",
            Self::BaseAction(BaseActionTask::V1(_)) => "args_action",
            Self::BaseAction(BaseActionTask::V2(_)) => "args_action_v2",
        }
    }
}

/// A trait representing a task that can be executed on Base layer
pub trait BaseTask: Send + Sync + Clone {
    /// Gets all pubkeys that involved in Task's instruction
    fn involved_accounts(&self, validator: &Pubkey) -> Vec<Pubkey> {
        self.instruction(validator)
            .accounts
            .iter()
            .map(|meta| meta.pubkey)
            .collect()
    }

    /// Gets target program for task execution
    fn program_id(&self) -> Pubkey;

    /// Gets instruction for task execution
    fn instruction(&self, validator: &Pubkey) -> Instruction;

    /// Attempts to optimize the task for smaller transaction size by switching
    /// to a buffer-based delivery. Returns `true` if optimization was applied.
    ///
    /// Deprecated: will be removed in the future. Optimization is a concern of
    /// the transaction strategist, not the task itself.
    fn try_optimize_tx_size(&mut self) -> bool;

    /// Returns [`Task`] budget
    fn compute_units(&self) -> u32;

    /// Returns the max accounts-data-size that can be used with SetLoadedAccountsDataSizeLimit
    fn accounts_size_budget(&self) -> u32;
}

#[derive(Clone, Debug)]
pub struct CommitDiffTask {
    pub commit_id: u64,
    pub allow_undelegation: bool,
    pub committed_account: CommittedAccount,
    pub base_account: Account,
}

#[derive(Clone, Debug)]
pub struct UndelegateTask {
    pub delegated_account: Pubkey,
    pub owner_program: Pubkey,
    pub rent_reimbursement: Pubkey,
}

impl UndelegateTask {
    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        dlp::instruction_builder::undelegate(
            *validator,
            self.delegated_account,
            self.owner_program,
            self.rent_reimbursement,
        )
    }
}

impl From<UndelegateTask> for BaseTaskImpl {
    fn from(value: UndelegateTask) -> Self {
        Self::Undelegate(value)
    }
}

#[derive(Clone, Debug)]
pub struct FinalizeTask {
    pub delegated_account: Pubkey,
}

impl FinalizeTask {
    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        dlp::instruction_builder::finalize(*validator, self.delegated_account)
    }
}

impl From<FinalizeTask> for BaseTaskImpl {
    fn from(value: FinalizeTask) -> Self {
        Self::Finalize(value)
    }
}

#[derive(Clone, Debug)]
pub enum BaseActionTask {
    V1(BaseActionTaskV1),
    V2(BaseActionTaskV2),
}

impl BaseActionTask {
    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        match self {
            Self::V1(value) => value.instruction(validator),
            Self::V2(value) => value.instruction(validator),
        }
    }

    pub fn action(&self) -> &BaseAction {
        match self {
            Self::V1(value) => &value.action,
            Self::V2(value) => &value.action,
        }
    }

    pub fn compute_units(&self) -> u32 {
        self.action().compute_units
    }

    pub fn extract_callback(&mut self) -> Option<BaseActionCallback> {
        match self {
            BaseActionTask::V1(value) => value.action.callback.take(),
            BaseActionTask::V2(value) => value.action.callback.take(),
        }
    }

    pub fn has_callback(&self) -> bool {
        match self {
            BaseActionTask::V1(value) => value.action.callback.is_some(),
            BaseActionTask::V2(value) => value.action.callback.is_some(),
        }
    }

    pub fn accounts_size_budget(&self) -> u32 {
        let action = self.action();
        // assume all other accounts are Small accounts.
        let other_accounts_budget = action.account_metas_per_program.len()
            as u32
            * AccountSizeClass::Small.size_budget();

        match self {
            Self::V1(_) => call_handler_size_budget(
                AccountSizeClass::Medium,
                other_accounts_budget,
            ),
            Self::V2(_) => call_handler_v2_size_budget(
                AccountSizeClass::Medium,
                AccountSizeClass::Medium,
                other_accounts_budget,
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BaseActionTaskV1 {
    pub action: BaseAction,
}

impl BaseActionTaskV1 {
    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        let action = &self.action;
        let account_metas = action
            .account_metas_per_program
            .iter()
            .map(|short_meta| AccountMeta {
                pubkey: short_meta.pubkey,
                is_writable: short_meta.is_writable,
                is_signer: false,
            })
            .collect();
        dlp::instruction_builder::call_handler(
            *validator,
            action.destination_program,
            action.escrow_authority,
            account_metas,
            CallHandlerArgs {
                data: action.data_per_program.data.clone(),
                escrow_index: action.data_per_program.escrow_index,
            },
        )
    }

    pub fn account_metas(&self) -> Vec<AccountMeta> {
        BaseActionTaskV1::account_metas_static(&self.action)
    }

    pub fn call_handler_args(&self) -> CallHandlerArgs {
        BaseActionTaskV1::call_handler_args_static(&self.action)
    }

    fn account_metas_static(action: &BaseAction) -> Vec<AccountMeta> {
        action
            .account_metas_per_program
            .iter()
            .map(|short_meta| AccountMeta {
                pubkey: short_meta.pubkey,
                is_writable: short_meta.is_writable,
                is_signer: false,
            })
            .collect()
    }

    fn call_handler_args_static(action: &BaseAction) -> CallHandlerArgs {
        CallHandlerArgs {
            data: action.data_per_program.data.clone(),
            escrow_index: action.data_per_program.escrow_index,
        }
    }
}

impl From<BaseActionTaskV1> for BaseActionTask {
    fn from(value: BaseActionTaskV1) -> Self {
        Self::V1(value)
    }
}

impl From<BaseActionTask> for BaseTaskImpl {
    fn from(value: BaseActionTask) -> Self {
        Self::BaseAction(value)
    }
}

#[derive(Clone, Debug)]
pub struct BaseActionTaskV2 {
    pub action: BaseAction,
    pub source_program: Pubkey,
}

impl BaseActionTaskV2 {
    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        let action = &self.action;
        dlp::instruction_builder::call_handler_v2(
            *validator,
            action.destination_program,
            self.source_program,
            action.escrow_authority,
            self.account_metas(),
            self.call_handler_args(),
        )
    }

    pub fn account_metas(&self) -> Vec<AccountMeta> {
        BaseActionTaskV1::account_metas_static(&self.action)
    }

    pub fn call_handler_args(&self) -> CallHandlerArgs {
        BaseActionTaskV1::call_handler_args_static(&self.action)
    }
}

impl From<BaseActionTaskV2> for BaseActionTask {
    fn from(value: BaseActionTaskV2) -> Self {
        Self::V2(value)
    }
}

#[derive(Clone, Debug)]
pub struct PreparationTask {
    pub commit_id: u64,
    pub pubkey: Pubkey,
    pub chunks: Chunks,

    // TODO(edwin): replace with reference once done
    pub committed_data: Vec<u8>,
}

impl PreparationTask {
    /// Returns initialization [`Instruction`]
    pub fn init_instruction(&self, authority: &Pubkey) -> Instruction {
        // // SAFETY: as object_length internally uses only already allocated or static buffers,
        // // and we don't use any fs writers, so the only error that may occur here is of kind
        // // OutOfMemory or WriteZero. This is impossible due to:
        // // Chunks::new panics if its size exceeds MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE or 10_240
        // // https://github.com/near/borsh-rs/blob/f1b75a6b50740bfb6231b7d0b1bd93ea58ca5452/borsh/src/ser/helpers.rs#L59
        let chunks_account_size =
            borsh::object_length(&self.chunks).unwrap() as u64;
        let buffer_account_size = self.committed_data.len() as u64;

        let (instruction, _, _) = create_init_ix(CreateInitIxArgs {
            authority: *authority,
            pubkey: self.pubkey,
            chunks_account_size,
            buffer_account_size,
            commit_id: self.commit_id,
            chunk_count: self.chunks.count(),
            chunk_size: self.chunks.chunk_size(),
        });

        instruction
    }

    /// Returns compute units required for realloc instruction
    pub fn init_compute_units(&self) -> u32 {
        12_000
    }

    /// Returns realloc instruction required for Buffer preparation
    #[allow(clippy::let_and_return)]
    pub fn realloc_instructions(&self, authority: &Pubkey) -> Vec<Instruction> {
        let buffer_account_size = self.committed_data.len() as u64;
        let realloc_instructions =
            create_realloc_buffer_ixs(CreateReallocBufferIxArgs {
                authority: *authority,
                pubkey: self.pubkey,
                buffer_account_size,
                commit_id: self.commit_id,
            });

        realloc_instructions
    }

    /// Returns compute units required for realloc instruction
    pub fn realloc_compute_units(&self) -> u32 {
        6_000
    }

    /// Returns realloc instruction required for Buffer preparation
    #[allow(clippy::let_and_return)]
    pub fn write_instructions(&self, authority: &Pubkey) -> Vec<Instruction> {
        let chunks_iter =
            ChangesetChunks::new(&self.chunks, self.chunks.chunk_size())
                .iter(&self.committed_data);
        let write_instructions = chunks_iter
            .map(|chunk| {
                create_write_ix(CreateWriteIxArgs {
                    authority: *authority,
                    pubkey: self.pubkey,
                    offset: chunk.offset,
                    data_chunk: chunk.data_chunk,
                    commit_id: self.commit_id,
                })
            })
            .collect::<Vec<_>>();

        write_instructions
    }

    pub fn write_compute_units(&self, bytes_count: usize) -> u32 {
        const PER_BYTE: u32 = 3;

        u32::try_from(bytes_count)
            .ok()
            .and_then(|bytes_count| bytes_count.checked_mul(PER_BYTE))
            .unwrap_or(u32::MAX)
    }

    pub fn chunks_pda(&self, authority: &Pubkey) -> Pubkey {
        pdas::chunks_pda(
            authority,
            &self.pubkey,
            self.commit_id.to_le_bytes().as_slice(),
        )
        .0
    }

    pub fn buffer_pda(&self, authority: &Pubkey) -> Pubkey {
        pdas::buffer_pda(
            authority,
            &self.pubkey,
            self.commit_id.to_le_bytes().as_slice(),
        )
        .0
    }

    pub fn cleanup_task(&self) -> CleanupTask {
        CleanupTask {
            pubkey: self.pubkey,
            commit_id: self.commit_id,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CleanupTask {
    pub pubkey: Pubkey,
    pub commit_id: u64,
}

impl CleanupTask {
    pub fn instruction(&self, authority: &Pubkey) -> Instruction {
        create_close_ix(CreateCloseIxArgs {
            authority: *authority,
            pubkey: self.pubkey,
            commit_id: self.commit_id,
        })
    }

    /// Returns compute units required to execute [`CleanupTask`]
    pub fn compute_units(&self) -> u32 {
        30_000
    }

    /// Returns a number of [`CleanupTask`]s that is possible to fit in single
    pub const fn max_tx_fit_count_with_budget() -> usize {
        8
    }

    pub fn chunks_pda(&self, authority: &Pubkey) -> Pubkey {
        pdas::chunks_pda(
            authority,
            &self.pubkey,
            self.commit_id.to_le_bytes().as_slice(),
        )
        .0
    }

    pub fn buffer_pda(&self, authority: &Pubkey) -> Pubkey {
        pdas::buffer_pda(
            authority,
            &self.pubkey,
            self.commit_id.to_le_bytes().as_slice(),
        )
        .0
    }
}

#[cfg(test)]
mod serialization_safety_test {

    use magicblock_program::{
        args::ShortAccountMeta, magic_scheduled_base_intent::ProgramArgs,
    };
    use solana_account::Account;

    use crate::{
        tasks::{
            commit_task::{CommitBufferStage, CommitDelivery, CommitTask},
            *,
        },
        test_utils,
    };

    fn setup() {
        test_utils::init_test_logger();
    }

    fn make_commit_task(
        commit_id: u64,
        allow_undelegation: bool,
        data: Vec<u8>,
        lamports: u64,
    ) -> CommitTask {
        CommitTask {
            commit_id,
            allow_undelegation,
            committed_account: CommittedAccount {
                pubkey: Pubkey::new_unique(),
                account: Account {
                    lamports,
                    data,
                    owner: Pubkey::new_unique(),
                    executable: false,
                    rent_epoch: 0,
                },
                remote_slot: Default::default(),
            },
            delivery_details: CommitDelivery::StateInArgs,
        }
    }

    #[test]
    fn test_args_task_instruction_serialization() {
        setup();
        let validator = Pubkey::new_unique();

        // Test Commit variant (StateInArgs)
        let commit_task: BaseTaskImpl =
            make_commit_task(123, true, vec![1, 2, 3], 1000).into();
        assert_serializable(&commit_task.instruction(&validator));

        // Test Finalize variant
        let finalize_task: BaseTaskImpl = FinalizeTask {
            delegated_account: Pubkey::new_unique(),
        }
        .into();
        assert_serializable(&finalize_task.instruction(&validator));

        // Test Undelegate variant
        let undelegate_task: BaseTaskImpl = UndelegateTask {
            delegated_account: Pubkey::new_unique(),
            owner_program: Pubkey::new_unique(),
            rent_reimbursement: Pubkey::new_unique(),
        }
        .into();
        assert_serializable(&undelegate_task.instruction(&validator));

        // Test BaseAction V1 variant
        let base_action: BaseTaskImpl = BaseActionTask::V1(BaseActionTaskV1 {
            action: BaseAction {
                destination_program: Pubkey::new_unique(),
                source_program: None,
                escrow_authority: Pubkey::new_unique(),
                account_metas_per_program: vec![ShortAccountMeta {
                    pubkey: Pubkey::new_unique(),
                    is_writable: true,
                }],
                data_per_program: ProgramArgs {
                    data: vec![4, 5, 6],
                    escrow_index: 1,
                },
                compute_units: 10_000,
            },
        })
        .into();
        assert_serializable(&base_action.instruction(&validator));

        // Test BaseAction V2 variant
        let base_action_v2: BaseTaskImpl =
            BaseActionTask::V2(BaseActionTaskV2 {
                action: BaseAction {
                    destination_program: Pubkey::new_unique(),
                    source_program: Some(Pubkey::new_unique()),
                    escrow_authority: Pubkey::new_unique(),
                    account_metas_per_program: vec![ShortAccountMeta {
                        pubkey: Pubkey::new_unique(),
                        is_writable: true,
                    }],
                    data_per_program: ProgramArgs {
                        data: vec![7, 8, 9],
                        escrow_index: 2,
                    },
                    compute_units: 15_000,
                },
                source_program: Pubkey::new_unique(),
            })
            .into();
        assert_serializable(&base_action_v2.instruction(&validator));
    }

    fn make_buffer_commit_task(
        commit_id: u64,
        allow_undelegation: bool,
        data: Vec<u8>,
        lamports: u64,
    ) -> CommitTask {
        let task =
            make_commit_task(commit_id, allow_undelegation, data, lamports);
        let stage = task.state_preparation_stage();
        CommitTask {
            delivery_details: CommitDelivery::StateInBuffer { stage },
            ..task
        }
    }

    #[test]
    fn test_buffer_task_instruction_serialization() {
        let validator = Pubkey::new_unique();

        let commit_task =
            make_buffer_commit_task(456, false, vec![7, 8, 9], 2000);
        assert!(commit_task.is_buffer());
        assert_serializable(&commit_task.instruction(&validator));
    }

    #[test]
    fn test_preparation_instructions_serialization() {
        let authority = Pubkey::new_unique();

        let commit_task =
            make_buffer_commit_task(789, true, vec![0; 1024], 3000);

        let Some(CommitBufferStage::Preparation(preparation_task)) =
            commit_task.stage()
        else {
            panic!("invalid preparation state on creation!");
        };
        assert_serializable(&preparation_task.init_instruction(&authority));
        for ix in preparation_task.realloc_instructions(&authority) {
            assert_serializable(&ix);
        }
        for ix in preparation_task.write_instructions(&authority) {
            assert_serializable(&ix);
        }
    }

    fn assert_serializable(ix: &Instruction) {
        bincode::serialize(ix).unwrap_or_else(|e| {
            panic!("Failed to serialize instruction {:?}: {}", ix, e)
        });
    }
}

#[test]
fn test_close_buffer_limit() {
    use solana_compute_budget_interface::ComputeBudgetInstruction;
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_transaction::Transaction;
    use tracing::info;

    use crate::{
        test_utils,
        transactions::{
            serialize_and_encode_base64, MAX_ENCODED_TRANSACTION_SIZE,
        },
    };

    test_utils::init_test_logger();

    let authority = Keypair::new();

    // Budget ixs (fixed)
    let compute_budget_ix =
        ComputeBudgetInstruction::set_compute_unit_limit(30_000);
    let compute_unit_price_ix =
        ComputeBudgetInstruction::set_compute_unit_price(101);

    // Each task unique: commit_id increments; pubkey is new_unique each time
    let base_commit_id = 101u64;
    let ixs_iter = (0..CleanupTask::max_tx_fit_count_with_budget()).map(|i| {
        let task = CleanupTask {
            commit_id: base_commit_id + i as u64,
            pubkey: Pubkey::new_unique(),
        };
        task.instruction(&authority.pubkey())
    });

    let mut ixs: Vec<_> = [compute_budget_ix, compute_unit_price_ix]
        .into_iter()
        .chain(ixs_iter)
        .collect();

    let tx = Transaction::new_with_payer(&ixs, Some(&authority.pubkey()));
    let tx_size = serialize_and_encode_base64(&tx).len();
    info!(transaction_size = tx_size, "Cleanup task transaction size");
    assert!(tx_size <= MAX_ENCODED_TRANSACTION_SIZE);

    // One more unique task should overflow
    let overflow_task = CleanupTask {
        commit_id: base_commit_id
            + CleanupTask::max_tx_fit_count_with_budget() as u64,
        pubkey: Pubkey::new_unique(),
    };
    ixs.push(overflow_task.instruction(&authority.pubkey()));

    let tx = Transaction::new_with_payer(&ixs, Some(&authority.pubkey()));
    assert!(
        serialize_and_encode_base64(&tx).len() > MAX_ENCODED_TRANSACTION_SIZE
    );
}
