use std::{collections::HashSet, ops::ControlFlow, sync::Arc, time::Duration};

use borsh::BorshDeserialize;
use futures_util::future::{join, join_all, try_join_all};
use light_client::indexer::{photon_indexer::PhotonIndexer, IndexerRpcConfig};
use log::{error, info};
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
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::{Instruction, InstructionError},
    message::{
        v0::Message, AddressLookupTableAccount, CompileError, VersionedMessage,
    },
    signature::{Keypair, Signature},
    signer::{Signer, SignerError},
    transaction::{TransactionError, VersionedTransaction},
};

use crate::{
    persist::{CommitStatus, IntentPersister},
    tasks::{
        task_builder::{get_compressed_data, TaskBuilderError},
        task_strategist::TransactionStrategy,
        BaseTask, BaseTaskError, BufferPreparationTask, CleanupTask,
        PreparationState, PreparationTask,
    },
    utils::persist_status_update,
    ComputeBudgetConfig,
};

#[derive(Clone)]
pub struct DeliveryPreparator {
    rpc_client: MagicblockRpcClient,
    table_mania: TableMania,
    compute_budget_config: ComputeBudgetConfig,
}

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
        photon_client: &Option<Arc<PhotonIndexer>>,
        commit_slot: Option<u64>,
    ) -> DeliveryPreparatorResult<Vec<AddressLookupTableAccount>> {
        let preparation_futures =
            strategy.optimized_tasks.iter_mut().map(|task| {
                let timer =
                    metrics::observe_committor_intent_task_preparation_time(
                        task.as_ref(),
                    );
                let res = self.prepare_task_handling_errors(
                    authority,
                    task,
                    persister,
                    photon_client,
                    commit_slot,
                );
                timer.stop_and_record();

                res
            });

        let task_preparations = join_all(preparation_futures);
        let alts_preparations = async {
            let timer =
                metrics::observe_committor_intent_alt_preparation_time();
            let res = self
                .prepare_lookup_tables(authority, &strategy.lookup_tables_keys)
                .await;
            timer.stop_and_record();

            res
        };

        let (res1, res2) = join(task_preparations, alts_preparations).await;
        res1.into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::FailedToPrepareBufferAccounts)?;
        let lookup_tables = res2.map_err(Error::FailedToCreateALTError)?;

        Ok(lookup_tables)
    }

    /// Prepares necessary parts for TX if needed, otherwise returns immediately
    pub async fn prepare_task<P: IntentPersister>(
        &self,
        authority: &Keypair,
        task: &mut dyn BaseTask,
        persister: &Option<P>,
        photon_client: &Option<Arc<PhotonIndexer>>,
        commit_slot: Option<u64>,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let PreparationState::Required(preparation_task) =
            task.preparation_state()
        else {
            return Ok(());
        };

        match preparation_task {
            PreparationTask::Buffer(buffer_info) => {
                // Persist as failed until rewritten
                let update_status =
                    CommitStatus::BufferAndChunkPartiallyInitialized;
                persist_status_update(
                    persister,
                    &buffer_info.pubkey,
                    buffer_info.commit_id,
                    update_status,
                );

                // Initialize buffer account. Init + reallocs
                self.initialize_buffer_account(authority, buffer_info)
                    .await?;

                // Persist initialization success
                let update_status = CommitStatus::BufferAndChunkInitialized;
                persist_status_update(
                    persister,
                    &buffer_info.pubkey,
                    buffer_info.commit_id,
                    update_status,
                );

                // Writing chunks with some retries
                self.write_buffer_with_retries(authority, buffer_info)
                    .await?;
                // Persist that buffer account initiated successfully
                let update_status =
                    CommitStatus::BufferAndChunkFullyInitialized;
                persist_status_update(
                    persister,
                    &buffer_info.pubkey,
                    buffer_info.commit_id,
                    update_status,
                );

                let cleanup_task = buffer_info.cleanup_task();
                task.switch_preparation_state(PreparationState::Cleanup(
                    cleanup_task,
                ))?;
            }
            PreparationTask::Compressed => {
                // Trying to fetch fresh data from the indexer
                let photon_config = commit_slot.map(|slot| IndexerRpcConfig {
                    slot,
                    ..Default::default()
                });

                let delegated_account = task
                    .delegated_account()
                    .ok_or(InternalError::DelegatedAccountNotFound)?;
                let photon_client = photon_client
                    .as_ref()
                    .ok_or(InternalError::PhotonClientNotFound)?;

                let compressed_data = get_compressed_data(
                    &delegated_account,
                    photon_client,
                    photon_config,
                )
                .await?;
                task.set_compressed_data(compressed_data);
            }
        }

        Ok(())
    }

    /// Runs `prepare_task` and, if the buffer was already initialized,
    /// performs cleanup and retries once.
    pub async fn prepare_task_handling_errors<P: IntentPersister>(
        &self,
        authority: &Keypair,
        task: &mut Box<dyn BaseTask>,
        persister: &Option<P>,
        photon_client: &Option<Arc<PhotonIndexer>>,
        commit_slot: Option<u64>,
    ) -> Result<(), InternalError> {
        let res = self
            .prepare_task(
                authority,
                task.as_mut(),
                persister,
                photon_client,
                commit_slot,
            )
            .await;
        match res {
            Err(InternalError::BufferExecutionError(
                BufferExecutionError::AccountAlreadyInitializedError(
                    err,
                    signature,
                ),
            )) => {
                info!(
                    "Buffer was already initialized prior: {}. {:?}",
                    err, signature
                );
            }
            // Return in any other case
            res => return res,
        }

        // Prepare buffer cleanup task
        let PreparationState::Required(PreparationTask::Buffer(
            preparation_task,
        )) = task.preparation_state().clone()
        else {
            return Ok(());
        };
        task.switch_preparation_state(PreparationState::Cleanup(
            preparation_task.cleanup_task(),
        ))?;
        self.cleanup(authority, std::slice::from_ref(task), &[])
            .await?;
        task.switch_preparation_state(PreparationState::Required(
            PreparationTask::Buffer(preparation_task),
        ))?;

        self.prepare_task(
            authority,
            task.as_mut(),
            persister,
            photon_client,
            commit_slot,
        )
        .await
    }

    /// Initializes buffer account for future writes
    #[allow(clippy::let_and_return)]
    async fn initialize_buffer_account(
        &self,
        authority: &Keypair,
        preparation_task: &BufferPreparationTask,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let authority_pubkey = authority.pubkey();
        let init_instruction =
            preparation_task.init_instruction(&authority_pubkey);
        let realloc_instructions =
            preparation_task.realloc_instructions(&authority_pubkey);

        let preparation_instructions =
            chunk_realloc_ixs(realloc_instructions, Some(init_instruction));
        let preparation_instructions = preparation_instructions
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
        self.send_ixs_with_retry(&preparation_instructions[0], authority, 5)
            .await?;

        // Reallocs can be performed in parallel
        let preparation_futs =
            preparation_instructions.iter().skip(1).map(|instructions| {
                self.send_ixs_with_retry(instructions, authority, 5)
            });
        try_join_all(preparation_futs).await?;

        Ok(())
    }

    async fn write_buffer_with_retries(
        &self,
        authority: &Keypair,
        preparation_task: &BufferPreparationTask,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let authority_pubkey = authority.pubkey();
        let chunks_pda = preparation_task.chunks_pda(&authority_pubkey);
        let write_instructions =
            preparation_task.write_instructions(&authority_pubkey);

        let chunks = if let Some(account) =
            self.rpc_client.get_account(&chunks_pda).await?
        {
            Ok(Chunks::try_from_slice(account.data())?)
        } else {
            error!(
                "Chunks PDA does not exist for writing. pda: {}",
                chunks_pda
            );
            Err(InternalError::ChunksPDAMissingError(chunks_pda))
        }?;

        self.write_missing_chunks(authority, &chunks, &write_instructions)
            .await
    }

    /// Extract & write missing chunks asynchronously
    async fn write_missing_chunks(
        &self,
        authority: &Keypair,
        chunks: &Chunks,
        write_instructions: &[Instruction],
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let missing_chunks = chunks.get_missing_chunks();
        let chunks_write_instructions = missing_chunks
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

        let fut_iter = chunks_write_instructions.iter().map(|instructions| {
            self.send_ixs_with_retry(instructions.as_slice(), authority, 5)
        });
        try_join_all(fut_iter).await?;

        Ok(())
    }

    // CommitProcessor::init_accounts analog
    async fn send_ixs_with_retry(
        &self,
        instructions: &[Instruction],
        authority: &Keypair,
        max_attempts: usize,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
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
                            err,
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

    /// Releases pubkeys from TableMania and
    /// cleans up after buffer tasks
    pub async fn cleanup(
        &self,
        authority: &Keypair,
        tasks: &[Box<dyn BaseTask>],
        lookup_table_keys: &[Pubkey],
    ) -> DeliveryPreparatorResult<(), InternalError> {
        self.table_mania
            .release_pubkeys(&HashSet::from_iter(
                lookup_table_keys.iter().cloned(),
            ))
            .await;

        let cleanup_tasks: Vec<_> = tasks
            .iter()
            .filter_map(|task| {
                if let PreparationState::Cleanup(cleanup_task) =
                    task.preparation_state()
                {
                    Some(cleanup_task)
                } else {
                    None
                }
            })
            .collect();

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
                    self.send_ixs_with_retry(&instructions, authority, 1).await
                }
            });

        join_all(close_futs)
            .await
            .into_iter()
            .map(|res| res.map_err(InternalError::from))
            .inspect(|res| {
                if let Err(err) = res {
                    error!("Failed to cleanup buffers: {}", err);
                }
            })
            .collect::<Result<(), _>>()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TransactionSendError {
    #[error("CompileError: {0}")]
    CompileError(#[from] CompileError),
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(#[from] MagicBlockRpcClientError),
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

impl From<MagicBlockRpcClientError> for BufferExecutionError {
    fn from(value: MagicBlockRpcClientError) -> Self {
        Self::TransactionSendError(
            TransactionSendError::MagicBlockRpcClientError(value),
        )
    }
}

#[derive(thiserror::Error, Debug)]
pub enum InternalError {
    #[error("Compressed data not found")]
    CompressedDataNotFound,
    #[error("0 retries was requested")]
    ZeroRetriesRequestedError,
    #[error("Chunks PDA does not exist for writing. pda: {0}")]
    ChunksPDAMissingError(Pubkey),
    #[error("BorshError: {0}")]
    BorshError(#[from] std::io::Error),
    #[error("TableManiaError: {0}")]
    TableManiaError(#[from] TableManiaError),
    #[error("TransactionCreationError: {0}")]
    TransactionCreationError(#[from] CompileError),
    #[error("TransactionSigningError: {0}")]
    TransactionSigningError(#[from] SignerError),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(#[from] MagicBlockRpcClientError),
    #[error("Delegated account not found")]
    DelegatedAccountNotFound,
    #[error("PhotonClientNotFound")]
    PhotonClientNotFound,
    #[error("TaskBuilderError: {0}")]
    TaskBuilderError(#[from] TaskBuilderError),
    #[error("BufferExecutionError: {0}")]
    BufferExecutionError(#[from] BufferExecutionError),
    #[error("BaseTaskError: {0}")]
    BaseTaskError(#[from] BaseTaskError),
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("FailedToPrepareBufferAccounts: {0}")]
    FailedToPrepareBufferAccounts(#[source] InternalError),
    #[error("FailedToCreateALTError: {0}")]
    FailedToCreateALTError(#[source] InternalError),
}

pub type DeliveryPreparatorResult<T, E = Error> = Result<T, E>;
