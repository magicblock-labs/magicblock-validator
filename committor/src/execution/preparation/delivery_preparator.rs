use std::{collections::HashSet, ops::ControlFlow, time::Duration};

use committor_interface::{Chunks, instruction_chunks::chunk_realloc_ixs};
use delegation_program_interface::{self as dpi, StateBuffer};
use futures_util::future::{join, join_all, try_join_all};
use metrics::metrics;
use rpc_client::{
    MagicBlockRpcClientError, MagicBlockSendTransactionConfig, MagicBlockSendTransactionOutcome,
    MagicblockRpcClient,
    utils::{
        SendErrorMapper, TransactionErrorMapper, decide_rpc_error_flow,
        map_magicblock_client_error, send_transaction_with_retries,
    },
};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{Instruction, error::InstructionError};
use solana_keypair::Keypair;
use solana_message::{AddressLookupTableAccount, CompileError, VersionedMessage, v0::Message};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::{Signer, SignerError};
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_error::TransactionError;
use table_mania::{TableMania, error::TableManiaError};
use tracing::{error, info};
use wheels::{DataLayoutError, layout::Decodable};

use crate::{
    ComputeBudgetConfig,
    execution::preparation::cleanup_resources::CleanupResources,
    shared::{
        account_state::DelegationProtocol,
        tasks::{
            Task,
            buffer_task::{BufferCleanup, BufferPreparation},
            task_strategist::TransactionStrategy,
            utils::TransactionUtils,
        },
    },
};

pub(crate) struct Preparator {
    rpc_client: MagicblockRpcClient,
    table_mania: TableMania,
    pub(super) compute_budget_config: ComputeBudgetConfig,
}

const MAX_PARALLEL_BUFFER_SENDS: usize = 8;

impl Preparator {
    pub(crate) fn new(
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
    pub(crate) async fn prepare_for_delivery(
        &self,
        authority: &Keypair,
        strategy: &TransactionStrategy,
        cleanup: &mut CleanupResources,
    ) -> DeliveryPreparatorResult<Vec<AddressLookupTableAccount>> {
        cleanup.uniqueness_nonce = strategy.uniqueness_nonce;
        // Record identities before preparation can be cancelled. Only successful
        // intent execution may close these buffers, including partial writes.
        cleanup.buffers = strategy
            .tasks
            .iter()
            .filter_map(|task| match task {
                Task::CommitFinalize(task) => BufferCleanup::from_commit_finalize(task),
                _ => None,
            })
            .collect();
        let uniqueness_nonce = strategy.uniqueness_nonce;
        let preparation_futures = strategy.tasks.iter().map(|task| async move {
            let _timer = metrics::observe_committor_intent_task_preparation_time(task);
            self.prepare_task_handling_errors(authority, task, uniqueness_nonce).await
        });

        let task_preparations = join_all(preparation_futures);
        let alts_preparations = async {
            let _timer = metrics::observe_committor_intent_alt_preparation_time();
            self.prepare_lookup_tables(
                authority,
                &strategy.lookup_tables_keys,
                &mut cleanup.alt_reservation,
            )
            .await
        };

        let (res1, res2) = join(task_preparations, alts_preparations).await;
        res1.into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(DeliveryPreparatorError::FailedToPrepareBufferAccounts)?;
        let lookup_tables = res2.map_err(DeliveryPreparatorError::FailedToCreateALTError)?;

        Ok(lookup_tables)
    }

    /// Prepares necessary parts for TX if needed, otherwise returns immediately
    pub(crate) async fn prepare_task(
        &self,
        authority: &Keypair,
        task: &Task,
        uniqueness_nonce: Option<u64>,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let preparation_task = match task {
            Task::CommitFinalize(commit_finalize_task) => {
                BufferPreparation::from_commit_finalize(commit_finalize_task)
            }
            _ => None,
        };
        let Some(preparation_task) = preparation_task else {
            return Ok(());
        };

        if matches!(preparation_task.protocol, DelegationProtocol::V2 { .. }) {
            return self.prepare_state_buffer(authority, &preparation_task, uniqueness_nonce).await;
        }

        // Initialize buffer account. Init + reallocs
        self.initialize_buffer_account(authority, &preparation_task, uniqueness_nonce)
            .await?;

        // Writing chunks with some retries
        self.write_buffer_with_retries(authority, &preparation_task, uniqueness_nonce)
            .await?;

        Ok(())
    }

    /// Resume a v2 upload from its validated prefix. DLP creates and grows the
    /// buffer while writing; each write must confirm before the next offset.
    async fn prepare_state_buffer(
        &self,
        authority: &Keypair,
        task: &BufferPreparation,
        uniqueness_nonce: Option<u64>,
    ) -> Result<(), InternalError> {
        if task.buffer_data.len() > StateBuffer::MAX_TOTAL_PAYLOAD_LEN as usize {
            return Err(InternalError::StateBufferTooLarge(task.buffer_data.len()));
        }
        let authority_pubkey = authority.pubkey();
        let (buffer_pda, bump) =
            dpi::state_buffer_pda_with_bump(&task.pubkey, task.commit_id, &authority_pubkey);
        let mut offset = 0;
        if let Some(account) = self.rpc_client.get_account(&buffer_pda).await?
            && !(account.owner == Pubkey::default()
                && account.data.is_empty()
                && !account.executable)
        {
            let invalid = || InternalError::InvalidStateBuffer(buffer_pda);
            if account.owner != dpi::id() || account.executable {
                return Err(invalid());
            }
            let buffer = StateBuffer::decode(&account.data).map_err(|_| invalid())?;
            let payload = buffer.payload();
            if buffer.discriminator() != StateBuffer::DISCRIMINATOR
                || buffer.bump() != bump
                || *buffer.authority() != authority_pubkey
                || *buffer.rent_payer() != authority_pubkey
                || *buffer.account_pubkey() != task.pubkey
                || buffer.commit_id() != task.commit_id
                || buffer.total_len() as usize != task.buffer_data.len()
                || !task.buffer_data.starts_with(payload.as_slice())
                || buffer.finalized() != (payload.len() == task.buffer_data.len())
            {
                return Err(invalid());
            }
            if buffer.finalized() {
                return Ok(());
            }
            offset = payload.len();
        }
        for instruction in task.write_instructions(&authority_pubkey, offset) {
            // Includes allocation/growth and hashing the full payload on the last
            // write. The legacy per-chunk copy budget does not cover these costs.
            let mut instructions = vec![
                ComputeBudgetInstruction::set_compute_unit_limit(200_000),
                ComputeBudgetInstruction::set_compute_unit_price(
                    self.compute_budget_config.compute_unit_price,
                ),
                instruction,
            ];
            self.send_ixs_with_retry(&mut instructions, authority, 5, uniqueness_nonce)
                .await?;
        }
        Ok(())
    }

    /// Runs `prepare_task` and, if the buffer was already initialized,
    /// performs cleanup and retries once.
    pub(crate) async fn prepare_task_handling_errors(
        &self,
        authority: &Keypair,
        task: &Task,
        uniqueness_nonce: Option<u64>,
    ) -> Result<(), InternalError> {
        let res = self.prepare_task(authority, task, uniqueness_nonce).await;
        match res {
            Err(InternalError::BufferExecution(
                BufferExecutionError::AccountAlreadyInitializedError(err, signature),
            )) => {
                info!(error = ?err, signature = ?signature, "Buffer already was initialized");
            }
            // Return in any other case
            res => return res,
        }

        // Preparation failed due to buffer existing - cleanup and retry
        let preparation_task = match task {
            Task::CommitFinalize(commit_finalize_task) => {
                BufferPreparation::from_commit_finalize(commit_finalize_task)
            }
            _ => None,
        };
        let Some(preparation_task) = preparation_task else {
            return Ok(());
        };

        // Cleanup
        let cleanup_task = preparation_task.cleanup_task();
        self.cleanup_buffers(authority, &[cleanup_task], uniqueness_nonce).await?;
        self.rpc_client.invalidate_cached_blockhash().await;

        // Retry preparation
        self.prepare_task(authority, task, uniqueness_nonce).await
    }

    /// Initializes buffer account for future writes
    async fn initialize_buffer_account(
        &self,
        authority: &Keypair,
        preparation_task: &BufferPreparation,
        uniqueness_nonce: Option<u64>,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
        let authority_pubkey = authority.pubkey();
        let init_instruction = preparation_task.init_instruction(&authority_pubkey);
        let realloc_instructions = preparation_task.realloc_instructions(&authority_pubkey);

        let preparation_instructions =
            chunk_realloc_ixs(realloc_instructions, Some(init_instruction));
        let mut preparation_instructions = preparation_instructions
            .into_iter()
            .enumerate()
            .map(|(i, ixs)| {
                let mut ixs_with_budget = if i == 0 {
                    self.compute_budget_config.buffer_init.instructions(ixs.len())
                } else {
                    self.compute_budget_config.buffer_realloc.instructions(ixs.len())
                };
                ixs_with_budget.extend(ixs);
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
        for batch in preparation_instructions[1..].chunks_mut(MAX_PARALLEL_BUFFER_SENDS) {
            let preparation_futs = batch.iter_mut().map(|instructions| {
                self.send_ixs_with_retry(instructions, authority, 5, uniqueness_nonce)
            });
            try_join_all(preparation_futs).await?;
        }

        Ok(())
    }

    /// Fills up initialized buffer
    async fn write_buffer_with_retries(
        &self,
        authority: &Keypair,
        preparation_task: &BufferPreparation,
        uniqueness_nonce: Option<u64>,
    ) -> DeliveryPreparatorResult<(), InternalError> {
        let authority_pubkey = authority.pubkey();
        let write_instructions = preparation_task.write_instructions(&authority_pubkey, 0);

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
                let mut instructions =
                    self.compute_budget_config.buffer_write.instructions(instruction.data.len());
                instructions.push(instruction);
                instructions
            })
            .collect::<Vec<_>>();

        for batch in chunks_write_instructions.chunks_mut(MAX_PARALLEL_BUFFER_SENDS) {
            let fut_iter = batch.iter_mut().map(|instructions| {
                self.send_ixs_with_retry(instructions, authority, 5, uniqueness_nonce)
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
            instructions.push(TransactionUtils::uniqueness_noop_instruction(nonce));
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
                    ) => Ok(BufferExecutionError::AccountAlreadyInitializedError(
                        err, signature,
                    )),
                    err => Err(err),
                }
            }
        }

        struct BufferErrorMapper<TxMap> {
            transaction_error_mapper: TxMap,
        }
        impl<TxMap> SendErrorMapper<TransactionSendError> for BufferErrorMapper<TxMap>
        where
            TxMap: TransactionErrorMapper<ExecutionError = BufferExecutionError>,
        {
            type ExecutionError = BufferExecutionError;
            fn map(&self, error: TransactionSendError) -> Self::ExecutionError {
                match error {
                    TransactionSendError::MagicBlockRpcClient(err) => {
                        map_magicblock_client_error(&self.transaction_error_mapper, *err)
                    }
                    err => BufferExecutionError::TransactionSendError(err),
                }
            }

            fn decide_flow(&self, err: &Self::ExecutionError) -> ControlFlow<(), Duration> {
                match err {
                    BufferExecutionError::TransactionSendError(
                        TransactionSendError::MagicBlockRpcClient(err),
                    ) => decide_rpc_error_flow(err),
                    _ => ControlFlow::Break(()),
                }
            }
        }

        let default_error_mapper = BufferErrorMapper {
            transaction_error_mapper: IntentTransactionErrorMapper,
        };
        let attempt = || async { self.try_send_ixs(instructions, authority).await };

        send_transaction_with_retries(attempt, default_error_mapper, |i, _| i >= max_attempts)
            .await?;
        Ok(())
    }

    async fn try_send_ixs(
        &self,
        instructions: &[Instruction],
        authority: &Keypair,
    ) -> DeliveryPreparatorResult<MagicBlockSendTransactionOutcome, TransactionSendError> {
        let latest_block_hash = self.rpc_client.get_latest_blockhash().await?;
        let message =
            Message::try_compile(&authority.pubkey(), instructions, &[], latest_block_hash)?;
        let transaction =
            VersionedTransaction::try_new(VersionedMessage::V0(message), &[authority])?;

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
        reserved: &mut HashSet<Pubkey>,
    ) -> DeliveryPreparatorResult<Vec<AddressLookupTableAccount>, InternalError> {
        if lookup_table_keys.is_empty() {
            return Ok(vec![]);
        }

        let pubkeys = HashSet::from_iter(lookup_table_keys.iter().copied());
        self.table_mania.reserve_pubkeys(authority, &pubkeys, reserved).await?;

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
    pub(crate) async fn cleanup(
        &self,
        authority: &Keypair,
        resources: &CleanupResources,
        close_buffers: bool,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
        let release_alts = async {
            if !resources.alt_reservation.is_empty() {
                self.table_mania.release_pubkeys(&resources.alt_reservation).await;
            }
        };

        if close_buffers {
            let (res, ()) = join(
                self.cleanup_buffers(authority, &resources.buffers, resources.uniqueness_nonce),
                release_alts,
            )
            .await;
            res
        } else {
            release_alts.await;
            Ok(())
        }
    }

    async fn cleanup_buffers(
        &self,
        authority: &Keypair,
        cleanup_tasks: &[BufferCleanup],
        uniqueness_nonce: Option<u64>,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
        if cleanup_tasks.is_empty() {
            return Ok(());
        }

        let close_futs = cleanup_tasks.chunks(BufferCleanup::MAX_PER_TRANSACTION).map(
            |cleanup_tasks| async move {
                let compute_units = BufferCleanup::COMPUTE_UNITS * cleanup_tasks.len() as u32;
                let mut instructions = vec![
                    ComputeBudgetInstruction::set_compute_unit_limit(compute_units),
                    ComputeBudgetInstruction::set_compute_unit_price(
                        self.compute_budget_config.compute_unit_price,
                    ),
                ];
                for task in cleanup_tasks {
                    instructions.push(task.instruction(&authority.pubkey())?);
                }
                self.send_ixs_with_retry(&mut instructions, authority, 1, uniqueness_nonce)
                    .await
                    .inspect_err(|err| {
                        let buffer_pdas = cleanup_tasks
                            .iter()
                            .map(|task| task.buffer_pda(&authority.pubkey()))
                            .collect::<Vec<_>>();
                        error!(error = ?err, "Failed to cleanup buffers: {:?}", buffer_pdas);
                    })
            },
        );

        join_all(close_futs).await.into_iter().collect()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TransactionSendError {
    #[error("CompileError: {0}")]
    Compile(#[from] CompileError),
    #[error("SignerError: {0}")]
    Signer(#[from] SignerError),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClient(Box<MagicBlockRpcClientError>),
}

impl From<MagicBlockRpcClientError> for TransactionSendError {
    fn from(e: MagicBlockRpcClientError) -> Self {
        Self::MagicBlockRpcClient(Box::new(e))
    }
}

impl TransactionSendError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::MagicBlockRpcClient(err) => err.signature(),
            _ => None,
        }
    }

    pub fn is_transient(&self) -> bool {
        match self {
            Self::Compile(_) | Self::Signer(_) => false,
            Self::MagicBlockRpcClient(err) => err.is_transient(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum BufferExecutionError {
    #[error("Failed to encode DLP buffer instruction: {0}")]
    InstructionEncoding(#[from] DataLayoutError),
    #[error("AccountAlreadyInitializedError: {0}")]
    AccountAlreadyInitializedError(#[source] TransactionError, Option<Signature>),
    #[error("TransactionSendError: {0}")]
    TransactionSendError(#[from] TransactionSendError),
}

impl BufferExecutionError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::InstructionEncoding(_) => None,
            Self::AccountAlreadyInitializedError(_, signature) => *signature,
            Self::TransactionSendError(err) => err.signature(),
        }
    }

    pub fn is_transient(&self) -> bool {
        match self {
            Self::InstructionEncoding(_) => false,
            // Leftover buffer from a prior attempt: the cleanup-and-retry in
            // `prepare_task_handling_errors` failed, a fresh attempt can heal
            Self::AccountAlreadyInitializedError(_, _) => true,
            Self::TransactionSendError(err) => err.is_transient(),
        }
    }
}

impl From<MagicBlockRpcClientError> for BufferExecutionError {
    fn from(value: MagicBlockRpcClientError) -> Self {
        Self::TransactionSendError(TransactionSendError::MagicBlockRpcClient(Box::new(value)))
    }
}

#[derive(thiserror::Error, Debug)]
pub enum InternalError {
    #[error("V2 StateBuffer payload is too large: {0} bytes")]
    StateBufferTooLarge(usize),
    #[error("V2 StateBuffer does not match the upload: {0}")]
    InvalidStateBuffer(Pubkey),
    #[error("0 retries was requested")]
    ZeroRetriesRequested,
    #[error("Chunks PDA does not exist for writing. pda: {0}")]
    ChunksPDAMissing(Pubkey),
    #[error("BorshError: {0}")]
    Borsh(#[from] std::io::Error),
    #[error("TableManiaError: {0}")]
    TableMania(#[from] TableManiaError),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClient(Box<MagicBlockRpcClientError>),
    #[error("BufferExecutionError: {0}")]
    BufferExecution(#[from] BufferExecutionError),
}

impl InternalError {
    /// A from-scratch re-preparation recreates missing chunk PDAs, so those
    /// count as transient alongside RPC and table-sync failures.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::StateBufferTooLarge(_)
            | Self::InvalidStateBuffer(_)
            | Self::ZeroRetriesRequested
            | Self::Borsh(_) => false,
            Self::ChunksPDAMissing(_) => true,
            Self::TableMania(err) => err.is_transient(),
            Self::MagicBlockRpcClient(err) => err.is_transient(),
            Self::BufferExecution(err) => err.is_transient(),
        }
    }
}

impl From<MagicBlockRpcClientError> for InternalError {
    fn from(e: MagicBlockRpcClientError) -> Self {
        Self::MagicBlockRpcClient(Box::new(e))
    }
}

impl InternalError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::MagicBlockRpcClient(err) => err.signature(),
            Self::BufferExecution(err) => err.signature(),
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
            Self::FailedToCreateALTError(err) | Self::FailedToPrepareBufferAccounts(err) => {
                err.signature()
            }
        }
    }

    pub fn is_transient(&self) -> bool {
        match self {
            Self::FailedToCreateALTError(err) | Self::FailedToPrepareBufferAccounts(err) => {
                err.is_transient()
            }
        }
    }
}

pub(crate) type DeliveryPreparatorResult<T, E = DeliveryPreparatorError> = Result<T, E>;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use json::{Value, json};
    use mbv_core::intent::types::CommittedAccount;
    use solana_account::Account;
    use solana_account_decoder::{UiAccountEncoding, encode_ui_account};
    use solana_rpc_client::{
        mock_sender::MockSender,
        nonblocking::rpc_client::RpcClient,
        rpc_sender::{RpcSender, RpcTransportStats},
    };
    use solana_rpc_client_api::{client_error::Result as RpcResult, request::RpcRequest};
    use solana_transaction_status_client_types::{EncodedTransaction, TransactionBinaryEncoding};
    use wheels::layout::Encodable;

    use super::*;
    use crate::shared::{
        tasks::{commit_delivery::CommitDelivery, utils::create_commit_finalize_task},
        transaction::{MAX_TRANSACTION_WIRE_SIZE, serialized_transaction_size},
    };

    struct Upload {
        buffer: StateBuffer,
        present: bool,
        awaiting_confirmation: bool,
        offsets: Vec<u32>,
        closes: usize,
    }

    struct BufferRpc {
        upload: Arc<Mutex<Upload>>,
        fallback: MockSender,
    }

    #[async_trait]
    impl RpcSender for BufferRpc {
        async fn send(&self, request: RpcRequest, params: Value) -> RpcResult<Value> {
            if request == RpcRequest::GetAccountInfo {
                let upload = self.upload.lock().unwrap();
                let account = upload.present.then(|| {
                    encode_ui_account(
                        &Pubkey::default(),
                        &Account {
                            owner: dpi::id(),
                            data: upload.buffer.encode().unwrap(),
                            lamports: 1,
                            ..Account::default()
                        },
                        UiAccountEncoding::Base64,
                        None,
                        None,
                    )
                });
                return Ok(json!({"context": {"slot": 1}, "value": account}));
            }
            if request == RpcRequest::SendTransaction {
                let transaction = EncodedTransaction::Binary(
                    params[0].as_str().unwrap().to_owned(),
                    TransactionBinaryEncoding::Base64,
                )
                .decode()
                .unwrap();
                assert!(serialized_transaction_size(&transaction) <= MAX_TRANSACTION_WIRE_SIZE);
                let keys = transaction.message.static_account_keys();
                let instruction = &transaction.message.instructions()[2];
                assert_eq!(keys[instruction.program_id_index as usize], dpi::id());
                {
                    let mut upload = self.upload.lock().unwrap();
                    assert!(
                        !upload.awaiting_confirmation,
                        "the previous write must confirm first"
                    );
                    upload.awaiting_confirmation = true;
                    match instruction.data[0] {
                        tag if tag == dpi::DlpV2Instruction::WriteStateBuffer as u8 => {
                            let args =
                                dpi::WriteStateBufferArgs::decode(&instruction.data[1..]).unwrap();
                            assert_eq!(args.offset() as usize, upload.buffer.payload.len());
                            assert_eq!(args.total_len(), upload.buffer.total_len);
                            upload.offsets.push(args.offset());
                            upload.buffer.payload.extend_from_slice(args.chunk());
                            upload.buffer.finalized =
                                upload.buffer.payload.len() == args.total_len() as usize;
                            upload.present = true;
                        }
                        tag if tag == dpi::DlpV2Instruction::CloseStateBuffer as u8 => {
                            let buffer = &upload.buffer;
                            assert_eq!(
                                keys[instruction.accounts[1] as usize],
                                dpi::state_buffer_pda(
                                    &buffer.account_pubkey,
                                    buffer.commit_id,
                                    &buffer.authority
                                )
                            );
                            assert_eq!(keys[instruction.accounts[3] as usize], buffer.rent_payer);
                            upload.closes += 1;
                            upload.present = false;
                        }
                        tag => panic!("unexpected DLP instruction {tag}"),
                    }
                }
                // Parallel writes would reach the assertion above before confirmation.
                tokio::task::yield_now().await;
            } else if request == RpcRequest::GetSignatureStatuses {
                self.upload.lock().unwrap().awaiting_confirmation = false;
            }
            self.fallback.send(request, params).await
        }

        fn get_transport_stats(&self) -> RpcTransportStats {
            RpcTransportStats::default()
        }
        fn url(&self) -> String {
            self.fallback.url()
        }
    }

    #[tokio::test]
    async fn v2_buffers_resume_in_order_and_close_after_success() {
        for use_diff in [false, true] {
            let authority = Keypair::new();
            let pubkey = Pubkey::new_unique();
            let mut task = create_commit_finalize_task(
                1,
                false,
                CommittedAccount {
                    pubkey,
                    account: Account {
                        data: vec![1; 2048],
                        ..Account::default()
                    },
                    remote_slot: 1,
                },
                None,
            );
            task.protocol = DelegationProtocol::V2 { cluster: Pubkey::new_unique() };
            task.delivery = if use_diff {
                CommitDelivery::DiffInBuffer {
                    base_account: Account {
                        data: vec![2; 2048],
                        ..Account::default()
                    },
                }
            } else {
                CommitDelivery::StateInBuffer
            };
            let data = BufferPreparation::from_commit_finalize(&task).unwrap().buffer_data;
            // Resume a partial diff, including a prefix that is not chunk-aligned.
            let prefix_len = if use_diff { 37 } else { 0 };
            let upload = Arc::new(Mutex::new(Upload {
                buffer: StateBuffer {
                    discriminator: StateBuffer::DISCRIMINATOR,
                    bump: dpi::state_buffer_pda_with_bump(&pubkey, 1, &authority.pubkey()).1,
                    authority: authority.pubkey(),
                    rent_payer: authority.pubkey(),
                    account_pubkey: pubkey,
                    commit_id: 1,
                    data_hash: [0; 32],
                    total_len: data.len() as u32,
                    finalized: false,
                    payload_alignment_padding: [0; 2],
                    payload: data[..prefix_len].to_vec(),
                },
                present: use_diff,
                awaiting_confirmation: false,
                offsets: vec![],
                closes: 0,
            }));
            let rpc = MagicblockRpcClient::from(RpcClient::new_sender(
                BufferRpc {
                    upload: upload.clone(),
                    fallback: MockSender::new("succeeds"),
                },
                Default::default(),
            ));
            let tables = TableMania::new(rpc.clone(), &authority, None);
            let preparator = Preparator::new(rpc, tables, ComputeBudgetConfig::new(0));
            let strategy = TransactionStrategy {
                tasks: vec![task.into()],
                ..Default::default()
            };
            let mut cleanup = CleanupResources::default();
            preparator
                .prepare_for_delivery(&authority, &strategy, &mut cleanup)
                .await
                .unwrap();
            let writes = {
                let upload = upload.lock().unwrap();
                assert_eq!(upload.buffer.payload, data);
                assert!(upload.buffer.finalized);
                assert_eq!(upload.offsets[0] as usize, prefix_len);
                upload.offsets.len()
            };
            preparator
                .prepare_for_delivery(&authority, &strategy, &mut cleanup)
                .await
                .unwrap();
            assert_eq!(
                upload.lock().unwrap().offsets.len(),
                writes,
                "a completed upload is reused"
            );

            upload.lock().unwrap().buffer.payload[0] ^= 1;
            assert!(matches!(
                preparator.prepare_task(&authority, &strategy.tasks[0], None).await,
                Err(InternalError::InvalidStateBuffer(_))
            ));
            upload.lock().unwrap().buffer.payload[0] ^= 1;
            preparator.cleanup(&authority, &cleanup, false).await.unwrap();
            assert!(
                upload.lock().unwrap().present,
                "an uncertain settlement must retain its buffer"
            );
            for _ in 0..2 {
                preparator.cleanup(&authority, &cleanup, true).await.unwrap();
            }
            assert!(!upload.lock().unwrap().present);
            assert_eq!(upload.lock().unwrap().closes, 2);
        }
    }
}
