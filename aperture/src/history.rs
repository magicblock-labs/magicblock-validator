use engine::Engine;
use ledger::request::{AccountSignaturesParams, BlockDetails, BlockParams, BlockResponse};
use magicblock_core::Slot;
use magicblock_ledger_deprecated::{Ledger, errors::LedgerError};
use solana_hash::Hash;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::response::RpcConfirmedTransactionStatusWithSignature;
use solana_signature::Signature;
use solana_transaction_error::TransactionError;
use solana_transaction_status::{
    BlockEncodingOptions, ConfirmedBlock, ConfirmedTransactionStatusWithSignature,
    ConfirmedTransactionWithStatusMeta, TransactionConfirmationStatus, TransactionStatus,
    UiConfirmedBlock, UiTransactionEncoding,
};
use std::{collections::HashSet, sync::Arc};
use tokio::{sync::Semaphore, task::block_in_place};

use crate::{
    RpcResult,
    error::{BLOCK_NOT_FOUND, RpcError},
    transaction::confirmed_transaction,
};

/// Engine-first historical reads and bounded, read-only legacy fallback.
pub(crate) struct History {
    engine: Engine,
    ledger: Arc<Ledger>,
    ledger_reads: Semaphore,
}

impl History {
    pub(crate) fn new(engine: Engine, ledger: Arc<Ledger>) -> Self {
        Self {
            engine,
            ledger,
            ledger_reads: Semaphore::new((num_cpus::get() / 4).max(1)),
        }
    }
    /// Bounds legacy disk work and keeps it off RPC workers. Engine reads and
    /// request validation must happen before admission. The permit remains held
    /// until the synchronous read and any enclosed encoding finish.
    ///
    /// Current-thread runtimes run inline because `block_in_place` is unsupported.
    async fn with_ledger<T, E>(
        &self,
        read: impl FnOnce(&Ledger) -> Result<T, E>,
    ) -> Result<T, RpcError>
    where
        E: Into<RpcError>,
    {
        use tokio::runtime::{Handle, RuntimeFlavor};

        let _permit = self
            .ledger_reads
            .acquire()
            .await
            .map_err(|_| RpcError::internal("legacy ledger read limiter closed"))?;
        if Handle::current().runtime_flavor() == RuntimeFlavor::MultiThread {
            block_in_place(|| read(&self.ledger)).map_err(Into::into)
        } else {
            read(&self.ledger).map_err(Into::into)
        }
    }
    /// Encode an Engine-first transaction, keeping legacy encoding inside its read permit.
    pub(crate) async fn transaction<T>(
        &self,
        signature: Signature,
        encode: impl FnOnce(Option<ConfirmedTransactionWithStatusMeta>) -> RpcResult<T>,
    ) -> RpcResult<T> {
        let Some(transaction) =
            self.engine.transactions().get(signature).await.map_err(RpcError::internal)?
        else {
            return self
                .with_ledger(|ledger| {
                    encode(ledger.get_complete_transaction(signature, Slot::MAX)?)
                })
                .await;
        };
        let slot = transaction.execution.header.slot;
        // Missing Engine block metadata does not trigger a separate legacy lookup.
        let block_time = self
            .engine
            .blocks()
            .get(BlockParams {
                slot,
                details: BlockDetails::None,
            })
            .await
            .map_err(RpcError::internal)?
            .map(|block| block.block().time);
        encode(Some(confirmed_transaction(transaction, block_time)?))
    }
    /// Encode the requested block details, falling back only when Engine has no record.
    pub(crate) async fn block(
        &self,
        slot: Slot,
        details: BlockDetails,
        encoding: UiTransactionEncoding,
        options: BlockEncodingOptions,
    ) -> RpcResult<Option<UiConfirmedBlock>> {
        let block = self
            .engine
            .blocks()
            .get(BlockParams { slot, details })
            .await
            .map_err(RpcError::internal)?;

        if let Some(block) = block {
            return encode_engine_block(block, encoding, options).map(Some);
        }
        self.with_ledger(|ledger| {
            ledger
                .get_block(slot)?
                .map(ConfirmedBlock::from)
                .map(|block| {
                    block.encode_with_options(encoding, options).map_err(|error| {
                        RpcError::internal(format!("failed to encode legacy block: {error}"))
                    })
                })
                .transpose()
        })
        .await
    }
    /// Resolve a retained block time or report a skipped/unavailable slot.
    pub(crate) async fn block_time(&self, block: Slot) -> RpcResult<i64> {
        let engine_block = self
            .engine
            .blocks()
            .get(BlockParams {
                slot: block,
                details: BlockDetails::None,
            })
            .await
            .map_err(RpcError::internal)?;
        if let Some(block) = engine_block {
            return Ok(block.block().time);
        }
        self.with_ledger(|ledger| ledger.get_block_time(block)).await?.ok_or_else(|| {
            RpcError::custom(
                format!("Slot {block} was skipped, or is not yet available"),
                BLOCK_NOT_FOUND,
            )
        })
    }
    /// Resolve statuses in request order with one legacy admission for all Engine misses.
    pub(crate) async fn statuses(
        &self,
        signatures: &[Signature],
    ) -> RpcResult<Vec<Option<TransactionStatus>>> {
        let mut statuses = Vec::with_capacity(signatures.len());

        for signature in signatures {
            // Level 1: Ask the engine, which owns the recent status cache.
            if let Some(status) = self
                .engine
                .transactions()
                .status(*signature)
                .await
                .map_err(RpcError::internal)?
            {
                statuses.push(Some(build_transaction_status(status.slot, status.result)));
                continue;
            }

            statuses.push(None);
        }

        // One legacy admission for the batch; engine-only results never wait on it.
        if statuses.iter().any(Option::is_none) {
            self.with_ledger(|ledger| {
                for (signature, status) in signatures.iter().zip(&mut statuses) {
                    if status.is_none() {
                        *status = ledger
                            .get_transaction_status(*signature, Slot::MAX)?
                            .map(|(slot, meta)| build_transaction_status(slot, meta.status));
                    }
                }
                Ok::<_, RpcError>(())
            })
            .await?;
        }

        Ok(statuses)
    }
    /// Whether an optional cursor has a retained Engine execution status.
    async fn contains_cursor(&self, signature: Option<Signature>) -> RpcResult<bool> {
        let Some(signature) = signature else {
            return Ok(false);
        };
        Ok(self
            .engine
            .transactions()
            .status(signature)
            .await
            .map_err(RpcError::internal)?
            .is_some())
    }

    /// Merge cursor-partitioned histories in descending slot/index order, deduplicating signatures.
    pub(crate) async fn signatures(
        &self,
        address: Pubkey,
        before: Option<Signature>,
        until: Option<Signature>,
        limit: usize,
    ) -> RpcResult<Vec<RpcConfirmedTransactionStatusWithSignature>> {
        // A cursor retained by the engine partitions the history: legacy data
        // is older than an engine `before` cursor and newer engine data is
        // excluded by a legacy `before` cursor. The same boundary is inverted
        // for `until`.
        let before_in_engine = self.contains_cursor(before).await?;
        let until_in_engine = self.contains_cursor(until).await?;

        let include_engine = before.is_none() || before_in_engine;
        let engine = if include_engine {
            self.engine
                .accounts()
                .signatures(AccountSignaturesParams {
                    pubkey: address,
                    limit,
                    before: before.filter(|_| before_in_engine),
                    until: until.filter(|_| until_in_engine),
                })
                .await
                .map_err(RpcError::internal)?
        } else {
            Vec::new()
        };

        let include_legacy = !until_in_engine;
        let legacy = if include_legacy {
            self.with_ledger(|ledger| {
                ledger.get_confirmed_signatures_for_address(
                    address,
                    Slot::MAX,
                    before.filter(|_| !before_in_engine),
                    until.filter(|_| !until_in_engine),
                    limit,
                )
            })
            .await?
            .infos
        } else {
            Vec::new()
        };

        let mut merged = engine
            .into_iter()
            .map(|info| {
                (
                    ConfirmedTransactionStatusWithSignature {
                        signature: info.signature,
                        slot: info.slot,
                        err: info.result.err(),
                        memo: None,
                        block_time: (info.blocktime != 0).then_some(info.blocktime),
                        // The engine does not retain an intra-block transaction index.
                        index: 0,
                    },
                    true,
                )
            })
            .chain(legacy.into_iter().map(|info| (info, false)))
            .collect::<Vec<_>>();
        // Keep Engine entries first on exact ties, then deduplicate before limiting
        // so overlapping records cannot consume the caller's result budget.
        merged.sort_by(|a, b| b.0.slot.cmp(&a.0.slot).then_with(|| b.0.index.cmp(&a.0.index)));
        let mut seen = HashSet::with_capacity(merged.len());
        merged.retain(|info| seen.insert(info.0.signature));
        merged.truncate(limit);

        let signatures = merged
            .into_iter()
            .map(|(info, from_engine)| {
                let mut rpc = RpcConfirmedTransactionStatusWithSignature::from(info);
                rpc.confirmation_status = Some(TransactionConfirmationStatus::Finalized);
                if from_engine {
                    // Preserve the documented engine placeholder instead of
                    // presenting a fabricated transaction index.
                    rpc.transaction_index = None;
                }
                rpc
            })
            .collect::<Vec<_>>();

        Ok(signatures)
    }
}

/// Encode Engine's available block fields, retaining placeholders for unavailable metadata.
fn encode_engine_block(
    response: BlockResponse,
    encoding: UiTransactionEncoding,
    options: BlockEncodingOptions,
) -> Result<UiConfirmedBlock, RpcError> {
    let block = *response.block();
    let base = UiConfirmedBlock {
        // Engine does not retain the previous blockhash on this RPC path.
        previous_blockhash: Hash::default().to_string(),
        blockhash: block.hash.to_string(),
        parent_slot: block.slot.saturating_sub(1),
        transactions: None,
        signatures: None,
        rewards: options.show_rewards.then(Vec::new),
        num_reward_partitions: None,
        block_time: Some(block.time),
        block_height: Some(block.slot),
    };
    match response {
        BlockResponse::Full(full) => {
            let transactions = full
                .transactions
                .into_iter()
                .map(|transaction| {
                    confirmed_transaction(transaction, Some(block.time))
                        .map(|transaction| transaction.tx_with_meta)
                })
                .collect::<RpcResult<Vec<_>>>()?;
            ConfirmedBlock {
                previous_blockhash: base.previous_blockhash,
                blockhash: base.blockhash,
                parent_slot: base.parent_slot,
                transactions,
                rewards: Vec::new(),
                num_partitions: None,
                block_time: base.block_time,
                block_height: base.block_height,
            }
            .encode_with_options(encoding, options)
            .map_err(|error| RpcError::internal(format!("failed to encode engine block: {error}")))
        }
        BlockResponse::WithSignatures(block) => Ok(UiConfirmedBlock {
            signatures: Some(
                block.signatures.into_iter().map(|signature| signature.to_string()).collect(),
            ),
            ..base
        }),
        BlockResponse::Bare(_) => Ok(base),
        BlockResponse::WithTransactions(_) => Err(RpcError::internal(
            "engine returned transaction-only block for an unsupported detail request",
        )),
    }
}

/// Report retained execution results as finalized, without a fabricated confirmation count.
fn build_transaction_status(slot: Slot, status: Result<(), TransactionError>) -> TransactionStatus {
    TransactionStatus {
        slot,
        status: status.clone(),
        confirmations: None,
        err: status.err(),
        confirmation_status: Some(TransactionConfirmationStatus::Finalized),
    }
}

impl From<LedgerError> for RpcError {
    fn from(value: LedgerError) -> Self {
        Self::internal(value)
    }
}
