use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use log::*;
use solana_rpc_client::{
    nonblocking::rpc_client::RpcClient, rpc_client::SerializableTransaction,
};
use solana_rpc_client_api::{
    client_error::ErrorKind as RpcClientErrorKind,
    config::{RpcSendTransactionConfig, RpcTransactionConfig},
    request::RpcError,
};
use solana_sdk::{
    account::Account,
    address_lookup_table::state::{AddressLookupTable, LookupTableMeta},
    clock::Slot,
    commitment_config::{CommitmentConfig, CommitmentLevel},
    hash::Hash,
    pubkey::Pubkey,
    signature::Signature,
    transaction::TransactionError,
};
use solana_transaction_error::TransactionResult;
use solana_transaction_status_client_types::{
    EncodedConfirmedTransactionWithStatusMeta, UiTransactionEncoding,
};
use tokio::task::JoinSet;

/// The encoding to use when sending transactions
pub const SEND_TRANSACTION_ENCODING: UiTransactionEncoding =
    UiTransactionEncoding::Base64;

/// The configuration to use when sending transactions
pub const SEND_TRANSACTION_CONFIG: RpcSendTransactionConfig =
    RpcSendTransactionConfig {
        preflight_commitment: None,
        skip_preflight: true,
        encoding: Some(SEND_TRANSACTION_ENCODING),
        max_retries: None,
        min_context_slot: None,
    };

// -----------------
// MagicBlockRpcClientError
// -----------------
#[derive(Debug, thiserror::Error)]
pub enum MagicBlockRpcClientError {
    #[error("RPC Client error: {0}")]
    RpcClientError(#[from] solana_rpc_client_api::client_error::Error),

    #[error("Error getting blockhash: {0} ({0:?})")]
    GetLatestBlockhash(solana_rpc_client_api::client_error::Error),

    #[error("Error getting slot: {0} ({0:?})")]
    GetSlot(solana_rpc_client_api::client_error::Error),

    #[error("Error deserializing lookup table: {0}")]
    LookupTableDeserialize(solana_sdk::instruction::InstructionError),

    #[error("Error sending transaction: {0} ({0:?})")]
    SendTransaction(solana_rpc_client_api::client_error::Error),

    #[error("Error getting signature status for: {0} {1}")]
    CannotGetTransactionSignatureStatus(Signature, String),

    #[error(
        "Error confirming signature status of {0} at desired commitment level {1}"
    )]
    CannotConfirmTransactionSignatureStatus(Signature, CommitmentLevel),

    #[error("Sent transaction {1} but got error: {0:?}")]
    SentTransactionError(TransactionError, Signature),
}

impl MagicBlockRpcClientError {
    /// Returns the signature of the transaction that caused the error
    /// if available.
    pub fn signature(&self) -> Option<Signature> {
        use MagicBlockRpcClientError::*;
        match self {
            CannotGetTransactionSignatureStatus(sig, _)
            | SentTransactionError(_, sig)
            | CannotConfirmTransactionSignatureStatus(sig, _) => Some(*sig),
            _ => None,
        }
    }
}

pub type MagicBlockRpcClientResult<T> =
    std::result::Result<T, MagicBlockRpcClientError>;

// -----------------
// SendAndConfirmTransaction Config and Outcome
// -----------------
pub enum MagicBlockSendTransactionConfig {
    /// Just send the transaction and return the signature.
    Send,
    /// Send a transaction and confirm it with the given parameters.
    SendAndConfirm {
        /// If provided we will wait for the given blockhash to become valid if
        /// getting the signature status fails due to `BlockhashNotFound`.
        wait_for_blockhash_to_become_valid: Option<Duration>,
        /// If provided we will try multiple time so find the signature status
        /// of the transaction at the 'processed' level even if the recent blockhash
        /// already became valid.
        wait_for_processed_level: Option<Duration>,
        /// How long to wait in between checks for processed commitment level.
        check_for_processed_interval: Option<Duration>,
        /// If provided it will wait for the transaction to be committed at the given
        /// commitment level. If not we just wait for the transaction to be processed and
        /// return the processed status.
        wait_for_commitment_level: Option<Duration>,
        /// How long to wait in between checks for desired commitment level.
        check_for_commitment_interval: Option<Duration>,
    },
}

// This seems rather large, but if we pick a lower value then test fail locally running
// against a (busy) solana test validator
// I verified that it actually takes this long for the transaction to become available
// in the explorer. Power settings on my machine actually affect this behavior.
const DEFAULT_MAX_TIME_TO_PROCESSED: Duration = Duration::from_millis(50_000);

impl MagicBlockSendTransactionConfig {
    // This will be used if we change the strategy for reallocs or writes
    #[allow(dead_code)]
    pub fn ensure_sent() -> Self {
        Self::Send
    }

    pub fn ensure_processed() -> Self {
        Self::SendAndConfirm {
            wait_for_blockhash_to_become_valid: Some(Duration::from_millis(
                2_000,
            )),
            wait_for_processed_level: Some(DEFAULT_MAX_TIME_TO_PROCESSED),
            check_for_processed_interval: Some(Duration::from_millis(400)),
            wait_for_commitment_level: None,
            check_for_commitment_interval: None,
        }
    }

    pub fn ensure_committed() -> Self {
        Self::SendAndConfirm {
            wait_for_blockhash_to_become_valid: Some(Duration::from_millis(
                2_000,
            )),
            wait_for_processed_level: Some(DEFAULT_MAX_TIME_TO_PROCESSED),
            check_for_processed_interval: Some(Duration::from_millis(400)),
            // NOTE: that this time is after we already verified that the transaction was
            //       processed
            wait_for_commitment_level: Some(Duration::from_millis(8_000)),
            check_for_commitment_interval: Some(Duration::from_millis(400)),
        }
    }

    pub fn ensures_committed(&self) -> bool {
        use MagicBlockSendTransactionConfig::*;
        match self {
            Send => false,
            SendAndConfirm {
                wait_for_commitment_level,
                ..
            } => wait_for_commitment_level.is_some(),
        }
    }
}

#[derive(Debug)]
pub struct MagicBlockSendTransactionOutcome {
    signature: Signature,
    processed_err: Option<TransactionError>,
    confirmed_err: Option<TransactionError>,
}

impl MagicBlockSendTransactionOutcome {
    pub fn into_signature(self) -> Signature {
        self.signature
    }

    pub fn into_signature_and_error(
        self,
    ) -> (Signature, Option<TransactionError>) {
        (self.signature, self.confirmed_err.or(self.processed_err))
    }

    /// Returns the error that occurred when processing the transaction.
    /// NOTE: this is never set if we use the [MagicBlockSendConfig::Send] option.
    pub fn error(&self) -> Option<&TransactionError> {
        self.confirmed_err.as_ref().or(self.processed_err.as_ref())
    }

    pub fn into_error(self) -> Option<TransactionError> {
        self.confirmed_err.or(self.processed_err)
    }

    pub fn into_result(self) -> Result<Signature, TransactionError> {
        if let Some(err) = self.confirmed_err.or(self.processed_err) {
            Err(err)
        } else {
            Ok(self.signature)
        }
    }
}

// -----------------
// MagicBlockRpcClient
// -----------------

// Derived from error from helius RPC: Failed to download accounts: Error { request: Some(GetMultipleAccounts), kind: RpcError(RpcResponseError { code: -32602, message: "Too many inputs provided; max 100", data: Empty }) }
const MAX_MULTIPLE_ACCOUNTS: usize = 100;

/// Wraps a [RpcClient] to provide improved functionality, specifically
/// for sending transactions.
#[derive(Clone)]
pub struct MagicblockRpcClient {
    client: Arc<RpcClient>,
}

impl From<RpcClient> for MagicblockRpcClient {
    fn from(client: RpcClient) -> Self {
        Self::new(Arc::new(client))
    }
}

impl MagicblockRpcClient {
    /// Create a new [MagicBlockRpcClient] from an existing [RpcClient].
    pub fn new(client: Arc<RpcClient>) -> Self {
        Self { client }
    }

    pub fn url(&self) -> String {
        self.client.url()
    }

    pub async fn get_latest_blockhash(
        &self,
    ) -> MagicBlockRpcClientResult<Hash> {
        self.client
            .get_latest_blockhash()
            .await
            .map_err(MagicBlockRpcClientError::GetLatestBlockhash)
    }

    pub async fn get_slot(&self) -> MagicBlockRpcClientResult<Slot> {
        self.client
            .get_slot()
            .await
            .map_err(MagicBlockRpcClientError::GetSlot)
    }

    pub async fn get_account(
        &self,
        pubkey: &Pubkey,
    ) -> MagicBlockRpcClientResult<Option<Account>> {
        let err = match self.client.get_account(pubkey).await {
            Ok(acc) => return Ok(Some(acc)),
            Err(err) => match err.kind() {
                RpcClientErrorKind::RpcError(rpc_err) => {
                    if let RpcError::ForUser(msg) = rpc_err {
                        if msg.starts_with("AccountNotFound") {
                            return Ok(None);
                        }
                    }
                    err
                }
                _ => err,
            },
        };
        Err(MagicBlockRpcClientError::RpcClientError(err))
    }
    pub async fn get_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
        max_per_fetch: Option<usize>,
    ) -> MagicBlockRpcClientResult<Vec<Option<Account>>> {
        self.get_multiple_accounts_with_commitment(
            pubkeys,
            self.commitment(),
            max_per_fetch,
        )
        .await
    }

    pub async fn get_multiple_accounts_with_commitment(
        &self,
        pubkeys: &[Pubkey],
        commitment: CommitmentConfig,
        max_per_fetch: Option<usize>,
    ) -> MagicBlockRpcClientResult<Vec<Option<Account>>> {
        let max_per_fetch = max_per_fetch.unwrap_or(MAX_MULTIPLE_ACCOUNTS);

        let mut join_set = JoinSet::new();
        for pubkey_chunk in pubkeys.chunks(max_per_fetch) {
            let client = self.client.clone();
            let pubkeys = pubkey_chunk.to_vec();
            join_set.spawn(async move {
                client
                    .get_multiple_accounts_with_commitment(&pubkeys, commitment)
                    .await
            });
        }
        let chunked_results = join_set.join_all().await;
        let mut results = Vec::new();
        for result in chunked_results {
            match result {
                Ok(accs) => results.extend(accs.value),
                Err(err) => {
                    return Err(MagicBlockRpcClientError::RpcClientError(err))
                }
            }
        }
        Ok(results)
    }

    pub async fn get_lookup_table_meta(
        &self,
        pubkey: &Pubkey,
    ) -> MagicBlockRpcClientResult<Option<LookupTableMeta>> {
        let acc = self.get_account(pubkey).await?;
        let Some(acc) = acc else { return Ok(None) };

        let table =
            AddressLookupTable::deserialize(&acc.data).map_err(|err| {
                MagicBlockRpcClientError::LookupTableDeserialize(err)
            })?;
        Ok(Some(table.meta))
    }

    pub async fn get_lookup_table_addresses(
        &self,
        pubkey: &Pubkey,
    ) -> MagicBlockRpcClientResult<Option<Vec<Pubkey>>> {
        let acc = self.get_account(pubkey).await?;
        let Some(acc) = acc else { return Ok(None) };

        let table =
            AddressLookupTable::deserialize(&acc.data).map_err(|err| {
                MagicBlockRpcClientError::LookupTableDeserialize(err)
            })?;
        Ok(Some(table.addresses.to_vec()))
    }

    pub async fn request_airdrop(
        &self,
        pubkey: &Pubkey,
        lamports: u64,
    ) -> MagicBlockRpcClientResult<Signature> {
        self.client
            .request_airdrop(pubkey, lamports)
            .await
            .map_err(MagicBlockRpcClientError::RpcClientError)
    }

    pub fn commitment(&self) -> CommitmentConfig {
        self.client.commitment()
    }

    pub fn commitment_level(&self) -> CommitmentLevel {
        self.commitment().commitment
    }

    pub async fn wait_for_next_slot(&self) -> MagicBlockRpcClientResult<Slot> {
        let slot = self.get_slot().await?;
        self.wait_for_higher_slot(slot).await
    }

    pub async fn wait_for_higher_slot(
        &self,
        slot: Slot,
    ) -> MagicBlockRpcClientResult<Slot> {
        let higher_slot = loop {
            let next_slot = self.get_slot().await?;
            if next_slot > slot {
                break next_slot;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        Ok(higher_slot)
    }

    /// Sends a transaction skipping preflight checks and then attempts to confirm
    /// it if so configured
    /// To confirm a transaction it uses the `client.commitment()` when requesting
    /// `get_signature_status_with_commitment`
    ///
    /// Does not support:
    /// - durable nonce transactions
    pub async fn send_transaction(
        &self,
        tx: &impl SerializableTransaction,
        config: &MagicBlockSendTransactionConfig,
    ) -> MagicBlockRpcClientResult<MagicBlockSendTransactionOutcome> {
        let sig = self
            .client
            .send_transaction_with_config(tx, SEND_TRANSACTION_CONFIG)
            .await
            .map_err(MagicBlockRpcClientError::SendTransaction)?;

        let MagicBlockSendTransactionConfig::SendAndConfirm {
            wait_for_processed_level,
            check_for_processed_interval,
            wait_for_blockhash_to_become_valid,
            wait_for_commitment_level,
            check_for_commitment_interval,
        } = config
        else {
            return Ok(MagicBlockSendTransactionOutcome {
                signature: sig,
                processed_err: None,
                confirmed_err: None,
            });
        };

        // 1. Wait for processed status
        let check_for_processed_interval = check_for_processed_interval
            .unwrap_or_else(|| Duration::from_millis(200));
        let wait_for_processed_level =
            wait_for_processed_level.unwrap_or_default();
        let processed_status = self
            .wait_for_processed_status(
                &sig,
                tx.get_recent_blockhash(),
                wait_for_processed_level,
                check_for_processed_interval,
                wait_for_blockhash_to_become_valid,
            )
            .await?;

        if let Err(err) = processed_status {
            return Err(MagicBlockRpcClientError::SentTransactionError(
                err, sig,
            ));
        }

        // 2. Wait for confirmed status if configured
        let confirmed_status = if let Some(wait_for_commitment_level) =
            wait_for_commitment_level
        {
            Some(
                self.wait_for_confirmed_status(
                    &sig,
                    wait_for_commitment_level,
                    check_for_commitment_interval,
                )
                .await?,
            )
        } else {
            None
        };

        Ok(MagicBlockSendTransactionOutcome {
            signature: sig,
            processed_err: processed_status.err(),
            confirmed_err: confirmed_status.and_then(|status| status.err()),
        })
    }

    /// Waits for a transaction to reach processed status
    pub async fn wait_for_processed_status(
        &self,
        signature: &Signature,
        recent_blockhash: &Hash,
        timeout: Duration,
        check_interval: Duration,
        blockhash_valid_timeout: &Option<Duration>,
    ) -> MagicBlockRpcClientResult<TransactionResult<()>> {
        let mut last_err =
            MagicBlockRpcClientError::CannotGetTransactionSignatureStatus(
                *signature,
                "blockhash was not found".into(),
            );

        let start = Instant::now();
        while start.elapsed() < timeout {
            let status = self
                .client
                .get_signature_status_with_commitment(
                    signature,
                    CommitmentConfig::processed(),
                )
                .await?;

            if let Some(status) = status {
                return Ok(status);
            }

            // Check if blockhash is still valid
            let Some(blockhash_valid_timeout) = blockhash_valid_timeout else {
                return Err(MagicBlockRpcClientError::CannotGetTransactionSignatureStatus(
                    *signature,
                    "timed out finding blockhash".to_string()
                ));
            };

            let blockhash_found = self
                .client
                .is_blockhash_valid(
                    recent_blockhash,
                    CommitmentConfig::processed(),
                )
                .await?;

            if !blockhash_found && &start.elapsed() < blockhash_valid_timeout {
                trace!(
                    "Waiting for blockhash {} to become valid",
                    recent_blockhash
                );
                tokio::time::sleep(Duration::from_millis(400)).await;
                continue;
            } else {
                last_err = MagicBlockRpcClientError::CannotGetTransactionSignatureStatus(
                    *signature,
                    format!("blockhash {} found", if blockhash_found {
                        "was"
                    } else {
                        "was not"
                    }),
                );
                tokio::time::sleep(check_interval).await;
            }
        }

        Err(last_err)
    }

    /// Waits for a transaction to reach confirmed status
    pub async fn wait_for_confirmed_status(
        &self,
        signature: &Signature,
        timeout: &Duration,
        check_interval: &Option<Duration>,
    ) -> MagicBlockRpcClientResult<TransactionResult<()>> {
        let start = Instant::now();
        let check_interval =
            check_interval.unwrap_or_else(|| Duration::from_millis(200));

        loop {
            let status = self
                .client
                .get_signature_status_with_commitment(
                    signature,
                    self.client.commitment(),
                )
                .await?;

            if let Some(status) = status {
                return Ok(status);
            }

            if &start.elapsed() < timeout {
                tokio::time::sleep(check_interval).await;
                continue;
            } else {
                return Err(MagicBlockRpcClientError::CannotConfirmTransactionSignatureStatus(
                    *signature,
                    self.client.commitment().commitment,
                ));
            }
        }
    }

    pub async fn get_transaction(
        &self,
        signature: &Signature,
        config: Option<RpcTransactionConfig>,
    ) -> MagicBlockRpcClientResult<EncodedConfirmedTransactionWithStatusMeta>
    {
        let config = config.unwrap_or_else(|| RpcTransactionConfig {
            commitment: Some(self.commitment()),
            ..Default::default()
        });
        self.client
            .get_transaction_with_config(signature, config)
            .await
            .map_err(MagicBlockRpcClientError::RpcClientError)
    }

    pub fn get_logs_from_transaction(
        tx: &EncodedConfirmedTransactionWithStatusMeta,
    ) -> Option<Vec<String>> {
        tx.transaction.meta.as_ref()?.log_messages.clone().into()
    }

    pub async fn get_transaction_logs(
        &self,
        signature: &Signature,
        config: Option<RpcTransactionConfig>,
    ) -> MagicBlockRpcClientResult<Option<Vec<String>>> {
        let tx = self.get_transaction(signature, config).await?;
        Ok(Self::get_logs_from_transaction(&tx))
    }

    pub fn get_cus_from_transaction(
        tx: &EncodedConfirmedTransactionWithStatusMeta,
    ) -> Option<u64> {
        tx.transaction
            .meta
            .as_ref()?
            .compute_units_consumed
            .clone()
            .into()
    }

    pub async fn get_transaction_cus(
        &self,
        signature: &Signature,
        config: Option<RpcTransactionConfig>,
    ) -> MagicBlockRpcClientResult<Option<u64>> {
        let tx = self.get_transaction(signature, config).await?;
        Ok(Self::get_cus_from_transaction(&tx))
    }

    pub fn get_inner(&self) -> &Arc<RpcClient> {
        &self.client
    }
}
