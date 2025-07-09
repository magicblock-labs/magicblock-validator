use std::{future::Future, ptr::write, sync::Arc, time::Duration};

use anyhow::anyhow;
use borsh::BorshDeserialize;
use futures_util::future::{join, join_all};
use log::{error, warn};
use magicblock_committor_program::{
    instruction_builder::{
        init_buffer::{create_init_ix, CreateInitIxArgs},
        realloc_buffer::{
            create_realloc_buffer_ixs, CreateReallocBufferIxArgs,
        },
    },
    Chunks, CommitableAccount,
};
use magicblock_rpc_client::{
    MagicBlockRpcClientError, MagicBlockSendTransactionConfig,
    MagicblockRpcClient,
};
use magicblock_table_mania::TableMania;
use solana_account::ReadableAccount;
use solana_rpc_client_api::client_error::reqwest::Version;
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    message::{
        v0::Message, AddressLookupTableAccount, CompileError, VersionedMessage,
    },
    signature::Keypair,
    signer::{Signer, SignerError},
    transaction::VersionedTransaction,
};
use tokio::{task::JoinSet, time::sleep};

use crate::{
    consts::MAX_WRITE_CHUNK_SIZE,
    error::{CommitAccountError, CommitAccountResult},
    persist::CommitStrategy,
    transaction_preperator::{
        error::PreparatorResult,
        task_builder::Task,
        task_strategist::{TaskDeliveryStrategy, TransactionStrategy},
        tasks::{CommitTask, TaskPreparationInfo},
    },
    CommitInfo, ComputeBudgetConfig,
};

// TODO: likely separate errors
type PreparationFuture = impl Future<Output = PreparatorResult<()>>;

pub struct DeliveryPreparationResult {
    lookup_tables: Vec<AddressLookupTableAccount>,
}

pub struct DeliveryPreparator {
    rpc_client: MagicblockRpcClient,
    table_mania: TableMania,
    compute_budget_config: ComputeBudgetConfig, // TODO(edwin): needed?
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
    pub async fn prepare_for_delivery(
        &self,
        authority: &Keypair,
        strategy: &TransactionStrategy,
    ) -> DeliveryPreparatorResult<()> {
        let preparation_futures = strategy
            .task_strategies
            .iter()
            .map(|task| self.prepare_task(authority, task));

        let fut1 = join_all(preparation_futures);
        let fut2 = if strategy.use_lookup_table {
            self.prepare_lookup_tables(&strategy.task_strategies)
        } else {
            std::future::ready(Ok(()))
        };
        let (res1, res2) = join(fut1, fut2).await;
        res1.into_iter().collect::<Result<Vec<_>, _>>()?;
        res2?;
        Ok(())
    }

    /// Prepares necessary parts for TX if needed, otherwise returns immediately
    // TODO(edwin): replace with interfaces
    async fn prepare_task(
        &self,
        authority: &Keypair,
        task: &TaskDeliveryStrategy,
    ) -> DeliveryPreparatorResult<()> {
        let TaskDeliveryStrategy::Buffer(task) = task else {
            return Ok(());
        };
        let Some(preparation_info) =
            task.get_preparation_instructions(authority)
        else {
            return Ok(());
        };

        // Initialize buffer account. Init + reallocs
        self.initialize_buffer_account(authority, task, &preparation_info)
            .await?;
        // Writing chunks with some retries. Stol
        self.write_buffer_with_retries::<5>(authority, &preparation_info)
            .await?;

        Ok(())
    }

    /// Initializes buffer account for future writes
    async fn initialize_buffer_account(
        &self,
        authority: &Keypair,
        task: &Task,
        preparation_info: &TaskPreparationInfo,
    ) -> DeliveryPreparatorResult<()> {
        let preparation_instructions =
            task.instructions_from_info(&preparation_info);
        let preparation_instructions = preparation_instructions
            .into_iter()
            .enumerate()
            .map(|(i, ixs)| {
                let mut ixs_with_budget = if i == 0 {
                    let init_budget_ixs = self
                        .compute_budget_config
                        .buffer_init
                        .instructions(ixs.len() - 1);
                    init_budget_ixs
                } else {
                    let realloc_budget_ixs = self
                        .compute_budget_config
                        .buffer_realloc
                        .instructions(ixs.len() - 1);
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
    async fn write_buffer_with_retries<const MAX_RETRIES: usize>(
        &self,
        authority: &Keypair,
        info: &TaskPreparationInfo,
    ) -> DeliveryPreparatorResult<()> {
        let mut last_error =
            Error::InternalError(anyhow!("ZeroRetriesRequested"));
        for _ in 0..MAX_RETRIES {
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
                        return Err(Error::InternalError(anyhow!(
                            "Chunks PDA does not exist for writing. pda: {}",
                            info.chunks_pda
                        )));
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
    ) -> DeliveryPreparatorResult<()> {
        if write_instructions.len() != chunks.count() {
            let err = anyhow!("Chunks count mismatches write instruction! chunks: {}, ixs: {}", write_instructions.len(), chunks.count());
            error!(err.to_string());
            return Err(Error::InternalError(err));
        }

        let mut join_set = JoinSet::new();
        let missing_chunks = chunks.get_missing_chunks();
        for missing_index in missing_chunks {
            let instruction = write_instructions[missing_index].clone();
            let mut instructions = self
                .compute_budget_config
                .buffer_write
                .instructions(instruction.data.len());
            instructions.push(instruction);

            join_set.spawn(async move {
                self.send_ixs_with_retry::<2>(&write_instructions, authority)
                    .await
                    .inspect_err(|err| {
                        error!("Error writing into buffect account: {:?}", err)
                    })
            });
        }

        join_set
            .join_all()
            .await
            .iter()
            .collect::<Result<Vec<_>, _>>()?;

        Ok(())
    }

    // TODO(edwin): move somewhere appropritate
    // CommitProcessor::init_accounts analog
    async fn send_ixs_with_retry<const MAX_RETRIES: usize>(
        &self,
        instructions: &[Instruction],
        authority: &Keypair,
    ) -> DeliveryPreparatorResult<()> {
        let mut last_error =
            Error::InternalError(anyhow!("ZeroRetriesRequested"));
        for _ in 0..MAX_RETRIES {
            match self.try_send_ixs(instructions, authority).await {
                Ok(()) => return Ok(()),
                Err(err) => last_error = err,
            }
            sleep(Duration::from_millis(200)).await;
        }

        Err(last_error)
    }

    async fn try_send_ixs(
        &self,
        instructions: &[Instruction],
        authority: &Keypair,
    ) -> DeliveryPreparatorResult<()> {
        let latest_block_hash = self.rpc_client.get_latest_blockhash().await?;
        let message = Message::try_compile(
            &authority.pubkey(),
            instructions,
            &vec![],
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

    async fn prepare_lookup_tables(
        &self,
        strategies: &[TaskDeliveryStrategy],
    ) -> DeliveryPreparatorResult<Vec<AddressLookupTableAccount>> {
        // self.table_mania.
        todo!()
    }
}

// TODO(edwin): properly define these for TransactionPreparator interface
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("InternalError: {0}")]
    InternalError(anyhow::Error),
    #[error("BorshError: {0}")]
    BorshError(#[from] std::io::Error),
    #[error("TransactionCreationError: {0}")]
    TransactionCreationError(#[from] CompileError),
    #[error("TransactionSigningError: {0]")]
    TransactionSigningError(#[from] SignerError),
    #[error("FailedToPrepareBufferError: {0}")]
    FailedToPrepareBufferError(#[from] MagicBlockRpcClientError),
}

pub type DeliveryPreparatorResult<T, E = Error> = Result<T, E>;
