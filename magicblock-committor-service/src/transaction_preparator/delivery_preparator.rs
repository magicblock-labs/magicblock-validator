use std::{collections::HashSet, ops::ControlFlow, time::Duration};

use futures_util::future::{join, join_all, try_join_all};
use magicblock_committor_program::{
    instruction_chunks::chunk_realloc_ixs, Chunks,
};
use magicblock_metrics::metrics;
use magicblock_rpc_client::{
    utils::{
        decide_rpc_error_flow, map_magicblock_client_error,
        send_transaction_with_retries, SendErrorMapper, TransactionErrorMapper,
    },
    MagicBlockRpcClientError, MagicBlockSendTransactionConfig,
    MagicBlockSendTransactionOutcome, MagicblockRpcClient,
};
use magicblock_table_mania::{error::TableManiaError, TableMania};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{error::InstructionError, Instruction};
use solana_keypair::Keypair;
use solana_message::{
    v0::Message, AddressLookupTableAccount, CompileError, VersionedMessage,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::{Signer, SignerError};
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_error::TransactionError;
use tracing::{error, info};

use crate::{
    persist::{CommitStatus, IntentPersister},
    tasks::{
        commit_stage_task::{CleanupTask, PreparationTask},
        task_strategist::TransactionStrategy,
        utils::TransactionUtils,
        BaseTaskImpl,
    },
    utils::persist_status_update,
    ComputeBudgetConfig,
};

pub struct DeliveryPreparator {
    rpc_client: MagicblockRpcClient,
    table_mania: TableMania,
    compute_budget_config: ComputeBudgetConfig,
}

const MAX_PARALLEL_BUFFER_SENDS: usize = 8;

impl DeliveryPreparator {
    pub fn new(
        rpc_client: MagicblockRpcClient,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
    ) -> Self {
        Self {
            rpc_client,
            table_mania,
            compute_budget_config,
        }
    }

    /// Prepares buffers and necessary pieces for optimized TX
    pub async fn prepare_for_delivery<P: IntentPersister>(
        &self,
        authority: &Keypair,
        strategy: &mut TransactionStrategy,
        persister: &Option<P>,
    ) -> DeliveryPreparatorResult<Vec<AddressLookupTableAccount>> {
        let uniqueness_nonce = strategy.uniqueness_nonce;
        let preparation_futures =
            strategy.optimized_tasks.iter_mut().map(|task| async move {
                let _timer =
                    metrics::observe_committor_intent_task_preparation_time(
                        &*task,
                    );
                self.prepare_task_handling_errors(
                    authority,
                    task,
                    persister,
                    uniqueness_nonce,
                )
                .await
            });

        let task_preparations = join_all(preparation_futures);
        let alts_preparations = async {
            let _timer =
                metrics::observe_committor_intent_alt_preparation_time();
            self.prepare_lookup_tables(authority, &strategy.lookup_tables_keys)
                .await
        };

        let (res1, res2) = join(task_preparations, alts_preparations).await;
        res1.into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(DeliveryPreparatorError::FailedToPrepareBufferAccounts)?;
        let lookup_tables =
            res2.map_err(DeliveryPreparatorError::FailedToCreateALTError)?;

        Ok(lookup_tables)
    }

    /// Prepares necessary parts for TX if needed, otherwise returns immediately
    pub async fn prepare_task<P: IntentPersister>(
        &self,
        authority: &Keypair,
        task: &mut BaseTaskImpl,
        persister: &Option<P>,
        uniqueness_nonce: Option<u64>,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let preparation_task = match task {
            BaseTaskImpl::Commit(commit_task) => {
                PreparationTask::from_commit(commit_task)
            }
            BaseTaskImpl::CommitFinalize(commit_finalize_task) => {
                PreparationTask::from_commit_finalize(commit_finalize_task)
            }
            _ => None,
        };
        let Some(preparation_task) = preparation_task else {
            return Ok(());
        };

        // Persist as failed until rewritten
        let update_status = CommitStatus::BufferAndChunkPartiallyInitialized;
        persist_status_update(
            persister,
            &preparation_task.pubkey,
            preparation_task.commit_id,
            update_status,
        );

        // Initialize buffer account. Init + reallocs
        self.initialize_buffer_account(
            authority,
            &preparation_task,
            uniqueness_nonce,
        )
        .await?;

        // Persist initialization success
        let update_status = CommitStatus::BufferAndChunkInitialized;
        persist_status_update(
            persister,
            &preparation_task.pubkey,
            preparation_task.commit_id,
            update_status,
        );

        // Writing chunks with some retries
        self.write_buffer_with_retries(
            authority,
            &preparation_task,
            uniqueness_nonce,
        )
        .await?;
        // Persist that buffer account initiated successfully
        let update_status = CommitStatus::BufferAndChunkFullyInitialized;
        persist_status_update(
            persister,
            &preparation_task.pubkey,
            preparation_task.commit_id,
            update_status,
        );

        preparation_task.done();
        Ok(())
    }

    /// Runs `prepare_task` and, if the buffer was already initialized,
    /// performs cleanup and retries once.
    pub async fn prepare_task_handling_errors<P: IntentPersister>(
        &self,
        authority: &Keypair,
        task: &mut BaseTaskImpl,
        persister: &Option<P>,
        uniqueness_nonce: Option<u64>,
    ) -> Result<(), InternalError> {
        let res = self
            .prepare_task(authority, task, persister, uniqueness_nonce)
            .await;
        match res {
            Err(InternalError::BufferExecutionError(
                BufferExecutionError::AccountAlreadyInitializedError(
                    err,
                    signature,
                ),
            )) => {
                info!(error = ?err, signature = ?signature, "Buffer already was initialized");
            }
            // Return in any other case
            res => return res,
        }

        // Preparation failed due to buffer existing - cleanup and retry
        let preparation_task = match task {
            BaseTaskImpl::Commit(commit_task) => {
                PreparationTask::from_commit(commit_task)
            }
            BaseTaskImpl::CommitFinalize(commit_finalize_task) => {
                PreparationTask::from_commit_finalize(commit_finalize_task)
            }
            _ => None,
        };
        let Some(preparation_task) = preparation_task else {
            return Ok(());
        };

        // Cleanup
        let cleanup_task = preparation_task.cleanup_task();
        self.cleanup_buffers(authority, &[cleanup_task], uniqueness_nonce)
            .await?;
        self.rpc_client.invalidate_cached_blockhash().await;

        // Retry preparation
        self.prepare_task(authority, task, persister, uniqueness_nonce)
            .await
    }

    /// Initializes buffer account for future writes
    #[allow(clippy::let_and_return)]
    async fn initialize_buffer_account(
        &self,
        authority: &Keypair,
        preparation_task: &PreparationTask<'_>,
        uniqueness_nonce: Option<u64>,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
        let authority_pubkey = authority.pubkey();
        let init_instruction =
            preparation_task.init_instruction(&authority_pubkey);
        let realloc_instructions =
            preparation_task.realloc_instructions(&authority_pubkey);

        let preparation_instructions =
            chunk_realloc_ixs(realloc_instructions, Some(init_instruction));
        let mut preparation_instructions = preparation_instructions
            .into_iter()
            .enumerate()
            .map(|(i, ixs)| {
                let mut ixs_with_budget = if i == 0 {
                    let init_budget_ixs = self
                        .compute_budget_config
                        .buffer_init
                        .instructions(ixs.len());
                    init_budget_ixs
                } else {
                    let realloc_budget_ixs = self
                        .compute_budget_config
                        .buffer_realloc
                        .instructions(ixs.len());
                    realloc_budget_ixs
                };
                ixs_with_budget.extend(ixs.into_iter());
                ixs_with_budget
            })
            .collect::<Vec<_>>();

        // Initialization
        self.send_ixs_with_retry(
            &mut preparation_instructions[0],
            authority,
            5,
            uniqueness_nonce,
        )
        .await?;

        // Reallocs can be performed in parallel
        for batch in
            preparation_instructions[1..].chunks_mut(MAX_PARALLEL_BUFFER_SENDS)
        {
            let preparation_futs = batch.iter_mut().map(|instructions| {
                self.send_ixs_with_retry(
                    instructions,
                    authority,
                    5,
                    uniqueness_nonce,
                )
            });
            try_join_all(preparation_futs).await?;
        }

        Ok(())
    }

    /// Fills up initialized buffer
    async fn write_buffer_with_retries(
        &self,
        authority: &Keypair,
        preparation_task: &PreparationTask<'_>,
        uniqueness_nonce: Option<u64>,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let authority_pubkey = authority.pubkey();
        let write_instructions =
            preparation_task.write_instructions(&authority_pubkey);

        self.write_missing_chunks(
            authority,
            &preparation_task.chunks,
            &write_instructions,
            uniqueness_nonce,
        )
        .await
    }

    /// Extract & write missing chunks asynchronously
    async fn write_missing_chunks(
        &self,
        authority: &Keypair,
        chunks: &Chunks,
        write_instructions: &[Instruction],
        uniqueness_nonce: Option<u64>,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let missing_chunks = chunks.get_missing_chunks();
        let mut chunks_write_instructions = missing_chunks
            .into_iter()
            .map(|missing_index| {
                let instruction = write_instructions[missing_index].clone();
                let mut instructions = self
                    .compute_budget_config
                    .buffer_write
                    .instructions(instruction.data.len());
                instructions.push(instruction);
                instructions
            })
            .collect::<Vec<_>>();

        for batch in
            chunks_write_instructions.chunks_mut(MAX_PARALLEL_BUFFER_SENDS)
        {
            let fut_iter = batch.iter_mut().map(|instructions| {
                self.send_ixs_with_retry(
                    instructions,
                    authority,
                    5,
                    uniqueness_nonce,
                )
            });
            try_join_all(fut_iter).await?;
        }

        Ok(())
    }

    // CommitProcessor::init_accounts analog
    async fn send_ixs_with_retry(
        &self,
        instructions: &mut Vec<Instruction>,
        authority: &Keypair,
        max_attempts: usize,
        uniqueness_nonce: Option<u64>,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
        if let Some(nonce) = uniqueness_nonce {
            instructions
                .push(TransactionUtils::uniqueness_noop_instruction(nonce));
        }

        /// Error mappers
        struct IntentTransactionErrorMapper;
        impl TransactionErrorMapper for IntentTransactionErrorMapper {
            type ExecutionError = BufferExecutionError;
            fn try_map(
                &self,
                error: TransactionError,
                signature: Option<Signature>,
            ) -> Result<Self::ExecutionError, TransactionError> {
                match error {
                    err @ TransactionError::InstructionError(
                        _,
                        InstructionError::AccountAlreadyInitialized,
                    ) => Ok(
                        BufferExecutionError::AccountAlreadyInitializedError(
                            err, signature,
                        ),
                    ),
                    err => Err(err),
                }
            }
        }

        struct BufferErrorMapper<TxMap> {
            transaction_error_mapper: TxMap,
        }
        impl<TxMap> SendErrorMapper<TransactionSendError> for BufferErrorMapper<TxMap>
        where
            TxMap:
                TransactionErrorMapper<ExecutionError = BufferExecutionError>,
        {
            type ExecutionError = BufferExecutionError;
            fn map(&self, error: TransactionSendError) -> Self::ExecutionError {
                match error {
                    TransactionSendError::MagicBlockRpcClientError(err) => {
                        map_magicblock_client_error(
                            &self.transaction_error_mapper,
                            *err,
                        )
                    }
                    err => BufferExecutionError::TransactionSendError(err),
                }
            }

            fn decide_flow(
                err: &Self::ExecutionError,
            ) -> ControlFlow<(), Duration> {
                match err {
                    BufferExecutionError::TransactionSendError(
                        TransactionSendError::MagicBlockRpcClientError(err),
                    ) => decide_rpc_error_flow(err),
                    _ => ControlFlow::Break(()),
                }
            }
        }

        let default_error_mapper = BufferErrorMapper {
            transaction_error_mapper: IntentTransactionErrorMapper,
        };
        let attempt =
            || async { self.try_send_ixs(instructions, authority).await };

        send_transaction_with_retries(attempt, default_error_mapper, |i, _| {
            i >= max_attempts
        })
        .await?;
        Ok(())
    }

    async fn try_send_ixs(
        &self,
        instructions: &[Instruction],
        authority: &Keypair,
    ) -> DeliveryPreparatorResult<
        MagicBlockSendTransactionOutcome,
        TransactionSendError,
    > {
        let latest_block_hash = self.rpc_client.get_latest_blockhash().await?;
        let message = Message::try_compile(
            &authority.pubkey(),
            instructions,
            &[],
            latest_block_hash,
        )?;
        let transaction = VersionedTransaction::try_new(
            VersionedMessage::V0(message),
            &[authority],
        )?;

        let outcome = self
            .rpc_client
            .send_transaction(
                &transaction,
                &MagicBlockSendTransactionConfig::ensure_committed(),
            )
            .await?;
        Ok(outcome)
    }

    /// Prepares ALTs for pubkeys participating in tx
    async fn prepare_lookup_tables(
        &self,
        authority: &Keypair,
        lookup_table_keys: &[Pubkey],
    ) -> DeliveryPreparatorResult<Vec<AddressLookupTableAccount>, InternalError>
    {
        if lookup_table_keys.is_empty() {
            return Ok(vec![]);
        }

        let pubkeys = HashSet::from_iter(lookup_table_keys.iter().copied());
        self.table_mania
            .reserve_pubkeys(authority, &pubkeys)
            .await?;

        let alts = self
            .table_mania
            .try_get_active_address_lookup_table_accounts(
                &pubkeys, // enough time for init/extend lookup table transaction to complete
                Duration::from_secs(50),
                // enough time for lookup table to finalize
                Duration::from_secs(50),
            )
            .await?;
        Ok(alts)
    }

    /// Releases pubkeys from TableMania and cleans up after buffer tasks.
    //
    // Cleaning buffers on failure isn't safe due to potential race condition:
    // Assume pubkey set A being committed
    // Intent1 fails and cleans up, another Intent2 with set A executes right away
    // That could lead for Intent2 init of buffers executing prior of Intent1 buffer cleanup
    // With same set A buffers will have same address
    //
    // To avoid this race on buffers we cleanup only succesfully executed intents
    // With intent retries all buffers will be eventually closed once intent succeeds
    pub async fn cleanup(
        &self,
        authority: &Keypair,
        cleanup_tasks: &[CleanupTask],
        lookup_table_keys: &[Pubkey],
        uniqueness_nonce: Option<u64>,
        close_buffers: bool,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
        let alt_set = HashSet::from_iter(lookup_table_keys.iter().cloned());
        let fut2 = self.table_mania.release_pubkeys(&alt_set);

        if close_buffers {
            let (res, ()) = join(
                self.cleanup_buffers(
                    authority,
                    cleanup_tasks,
                    uniqueness_nonce,
                ),
                fut2,
            )
            .await;
            res
        } else {
            fut2.await;
            Ok(())
        }
    }

    async fn cleanup_buffers(
        &self,
        authority: &Keypair,
        cleanup_tasks: &[CleanupTask],
        uniqueness_nonce: Option<u64>,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
        if cleanup_tasks.is_empty() {
            return Ok(());
        }

        let close_futs = cleanup_tasks
            .chunks(CleanupTask::max_tx_fit_count_with_budget())
            .map(|cleanup_tasks| {
                let compute_units = cleanup_tasks[0].compute_units()
                    * cleanup_tasks.len() as u32;
                let mut instructions = vec![
                    ComputeBudgetInstruction::set_compute_unit_limit(
                        compute_units,
                    ),
                    ComputeBudgetInstruction::set_compute_unit_price(
                        self.compute_budget_config.compute_unit_price,
                    ),
                ];
                instructions.extend(
                    cleanup_tasks
                        .iter()
                        .map(|task| task.instruction(&authority.pubkey())),
                );

                async move {
                    (
                        cleanup_tasks,
                        self.send_ixs_with_retry(
                            &mut instructions,
                            authority,
                            1,
                            uniqueness_nonce,
                        )
                        .await,
                    )
                }
            });

        join_all(close_futs)
            .await
            .into_iter()
            .try_for_each(|(cleanup_tasks, res)| {
                res.inspect_err(|err| {
                    let buffer_pdas = cleanup_tasks
                        .iter()
                        .map(|el| el.buffer_pda(&authority.pubkey()))
                        .collect::<Vec<_>>();
                    error!(error = ?err, "Failed to cleanup buffers: {:?}", buffer_pdas);
                })
            })
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TransactionSendError {
    #[error("CompileError: {0}")]
    CompileError(#[from] CompileError),
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(Box<MagicBlockRpcClientError>),
}

impl From<MagicBlockRpcClientError> for TransactionSendError {
    fn from(e: MagicBlockRpcClientError) -> Self {
        Self::MagicBlockRpcClientError(Box::new(e))
    }
}

impl TransactionSendError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::MagicBlockRpcClientError(err) => err.signature(),
            _ => None,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum BufferExecutionError {
    #[error("AccountAlreadyInitializedError: {0}")]
    AccountAlreadyInitializedError(
        #[source] TransactionError,
        Option<Signature>,
    ),
    #[error("TransactionSendError: {0}")]
    TransactionSendError(#[from] TransactionSendError),
}

impl BufferExecutionError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::AccountAlreadyInitializedError(_, signature) => *signature,
            Self::TransactionSendError(err) => err.signature(),
        }
    }
}

impl From<MagicBlockRpcClientError> for BufferExecutionError {
    fn from(value: MagicBlockRpcClientError) -> Self {
        Self::TransactionSendError(
            TransactionSendError::MagicBlockRpcClientError(Box::new(value)),
        )
    }
}

#[derive(thiserror::Error, Debug)]
pub enum InternalError {
    #[error("0 retries was requested")]
    ZeroRetriesRequestedError,
    #[error("Chunks PDA does not exist for writing. pda: {0}")]
    ChunksPDAMissingError(Pubkey),
    #[error("BorshError: {0}")]
    BorshError(#[from] std::io::Error),
    #[error("TableManiaError: {0}")]
    TableManiaError(#[from] TableManiaError),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(Box<MagicBlockRpcClientError>),
    #[error("BufferExecutionError: {0}")]
    BufferExecutionError(#[from] BufferExecutionError),
}

impl From<MagicBlockRpcClientError> for InternalError {
    fn from(e: MagicBlockRpcClientError) -> Self {
        Self::MagicBlockRpcClientError(Box::new(e))
    }
}

impl InternalError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::MagicBlockRpcClientError(err) => err.signature(),
            Self::BufferExecutionError(err) => err.signature(),
            _ => None,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum DeliveryPreparatorError {
    #[error("FailedToPrepareBufferAccounts: {0}")]
    FailedToPrepareBufferAccounts(#[source] InternalError),
    #[error("FailedToCreateALTError: {0}")]
    FailedToCreateALTError(#[source] InternalError),
}

impl DeliveryPreparatorError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::FailedToCreateALTError(err)
            | Self::FailedToPrepareBufferAccounts(err) => err.signature(),
        }
    }

    pub fn is_transient(&self) -> bool {
        match self {
            Self::FailedToCreateALTError(err)
            | Self::FailedToPrepareBufferAccounts(err) => err.is_transient(),
        }
    }
}

pub type DeliveryPreparatorResult<T, E = DeliveryPreparatorError> =
    Result<T, E>;
