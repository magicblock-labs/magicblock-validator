use std::{collections::HashSet, time::Duration};

use borsh::BorshDeserialize;
use futures_util::future::{join, join_all};
use light_client::indexer::photon_indexer::PhotonIndexer;
use log::*;
use magicblock_committor_program::{
    instruction_chunks::chunk_realloc_ixs, Chunks,
};
use magicblock_rpc_client::{
    MagicBlockRpcClientError, MagicBlockSendTransactionConfig,
    MagicblockRpcClient,
};
use magicblock_table_mania::{error::TableManiaError, TableMania};
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;
use solana_sdk::{
    instruction::Instruction,
    message::{
        v0::Message, AddressLookupTableAccount, CompileError, VersionedMessage,
    },
    signature::Keypair,
    signer::{Signer, SignerError},
    transaction::VersionedTransaction,
};
use tokio::time::sleep;

use crate::{
    persist::{CommitStatus, IntentPersister},
    tasks::{
        task_builder::{get_compressed_data, TaskBuilderError},
        task_strategist::TransactionStrategy,
        BaseTask, BufferPreparationInfo, TaskPreparationInfo,
    },
    utils::persist_status_update,
    ComputeBudgetConfig,
};

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
        photon_client: &Option<PhotonIndexer>,
    ) -> DeliveryPreparatorResult<Vec<AddressLookupTableAccount>> {
        let preparation_futures =
            strategy.optimized_tasks.iter_mut().map(|task| {
                self.prepare_task(
                    authority,
                    task.as_mut(),
                    persister,
                    photon_client,
                )
            });

        let task_preparations = join_all(preparation_futures);
        let alts_preparations =
            self.prepare_lookup_tables(authority, &strategy.lookup_tables_keys);

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
        photon_client: &Option<PhotonIndexer>,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let Some(preparation_info) = task.preparation_info(&authority.pubkey())
        else {
            return Ok(());
        };

        match preparation_info {
            TaskPreparationInfo::Buffer(buffer_info) => {
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
                self.initialize_buffer_account(authority, &buffer_info)
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
                self.write_buffer_with_retries(authority, &buffer_info, 5)
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
            }
            TaskPreparationInfo::Compressed => {
                // HACK: We retry until the hash changes to be sure that the indexer has the change.
                // This is a bad way of doing it as it assumes that the hash changes.
                // It will break if the action is done in an isolated manner.
                let original_hash = task
                    .get_compressed_data()
                    .expect("Compressed data not found")
                    .hash;
                let delegated_account = task
                    .delegated_account()
                    .ok_or(InternalError::DelegatedAccountNotFound)?;
                let photon_client = photon_client
                    .as_ref()
                    .ok_or(InternalError::PhotonClientNotFound)?;

                // HACK: The indexer takes some time, so we retry a few times to be sure that the hash is updated.
                // In the case where the hash is not supposed to change, we will have to do max retry, which is bad.
                let mut retries = 10;
                let compressed_data = loop {
                    let compressed_data =
                        get_compressed_data(&delegated_account, &photon_client)
                            .await?;

                    if compressed_data.hash != original_hash || retries == 0 {
                        break compressed_data;
                    }

                    sleep(Duration::from_millis(100)).await;
                    retries -= 1;
                };
                task.set_compressed_data(compressed_data);
            }
        }

        Ok(())
    }

    /// Initializes buffer account for future writes
    #[allow(clippy::let_and_return)]
    async fn initialize_buffer_account(
        &self,
        authority: &Keypair,
        info: &BufferPreparationInfo,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let preparation_instructions = chunk_realloc_ixs(
            info.realloc_instructions.clone(),
            Some(info.init_instruction.clone()),
        );
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

        // Initialization & reallocs
        for instructions in preparation_instructions {
            self.send_ixs_with_retry::<2>(&instructions, authority)
                .await?;
        }

        Ok(())
    }

    /// Based on Chunks state, try MAX_RETRIES to fill buffer
    async fn write_buffer_with_retries(
        &self,
        authority: &Keypair,
        info: &BufferPreparationInfo,
        max_retries: usize,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let mut last_error = InternalError::ZeroRetriesRequestedError;
        for _ in 0..max_retries {
            let chunks =
                match self.rpc_client.get_account(&info.chunks_pda).await {
                    Ok(Some(account)) => {
                        Chunks::try_from_slice(account.data())?
                    }
                    Ok(None) => {
                        error!(
                            "Chunks PDA does not exist for writing. pda: {}",
                            info.chunks_pda
                        );
                        return Err(InternalError::ChunksPDAMissingError(
                            info.chunks_pda,
                        ));
                    }
                    Err(err) => {
                        error!("Failed to fetch chunks PDA: {:?}", err);
                        last_error = err.into();
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };

            match self
                .write_missing_chunks(
                    authority,
                    &chunks,
                    &info.write_instructions,
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(err) => {
                    error!("Error on write missing chunks attempt: {:?}", err);
                    last_error = err
                }
            }
        }

        Err(last_error)
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
            self.send_ixs_with_retry::<2>(instructions.as_slice(), authority)
        });

        join_all(fut_iter)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;

        Ok(())
    }

    // CommitProcessor::init_accounts analog
    async fn send_ixs_with_retry<const MAX_RETRIES: usize>(
        &self,
        instructions: &[Instruction],
        authority: &Keypair,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let mut last_error = InternalError::ZeroRetriesRequestedError;
        for _ in 0..MAX_RETRIES {
            match self.try_send_ixs(instructions, authority).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    println!("Failed attempt to send tx: {:?}", err);
                    last_error = err;
                }
            }
            sleep(Duration::from_millis(200)).await;
        }

        Err(last_error)
    }

    async fn try_send_ixs(
        &self,
        instructions: &[Instruction],
        authority: &Keypair,
    ) -> DeliveryPreparatorResult<(), InternalError> {
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

        self.rpc_client
            .send_transaction(
                &transaction,
                &MagicBlockSendTransactionConfig::ensure_committed(),
            )
            .await?;
        Ok(())
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

    // TODO(edwin): cleanup
    // async fn clean() {
    //     todo!()
    // }
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
    #[error("TransactionCreationError: {0}")]
    TransactionCreationError(#[from] CompileError),
    #[error("TransactionSigningError: {0}")]
    TransactionSigningError(#[from] SignerError),
    #[error("FailedToPrepareBufferError: {0}")]
    FailedToPrepareBufferError(#[from] MagicBlockRpcClientError),
    #[error("InvalidPreparationInfo")]
    InvalidPreparationInfo,
    #[error("Delegated account not found")]
    DelegatedAccountNotFound,
    #[error("Photon client not found")]
    PhotonClientNotFound,
    #[error("Failed to prepare compressed data: {0}")]
    TaskBuilderError(#[from] TaskBuilderError),
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("FailedToPrepareBufferAccounts: {0}")]
    FailedToPrepareBufferAccounts(#[source] InternalError),
    #[error("FailedToCreateALTError: {0}")]
    FailedToCreateALTError(#[source] InternalError),
}

pub type DeliveryPreparatorResult<T, E = Error> = Result<T, E>;
