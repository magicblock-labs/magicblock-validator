use std::{collections::HashMap, num::NonZeroUsize, time::Duration};

use async_trait::async_trait;
use delegation_program_interface::{
    self as dpi, delegation_metadata_seeds_from_delegated_account,
    delegation_record_seeds_from_delegated_account,
    legacy::state::{DelegationMetadata, DelegationRecord},
};
use metrics::metrics;
use rpc_client::{MagicBlockRpcClientError, MagicblockRpcClient};
use solana_account::Account;
use solana_account_decoder::UiAccountEncoding;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    client_error::ErrorKind, config::RpcAccountInfoConfig,
    custom_error::JSON_RPC_SERVER_ERROR_MIN_CONTEXT_SLOT_NOT_REACHED, request::RpcError,
};
use solana_signature::Signature;
use tracing::{error, info, warn};

const NUM_FETCH_RETRIES: NonZeroUsize = NonZeroUsize::new(5).unwrap();

/// A delegated account paired with the base-layer slot of the state
/// snapshot being committed for it (`CommittedAccount::remote_slot`).
/// The slot identifies the delegation session; a different record stamp
/// belongs to another delegation of the same account.
pub type AccountSnapshot = (Pubkey, u64);

#[async_trait]
pub(crate) trait TaskInfoProvider: Send + Sync + 'static {
    /// Fetches delegation metadata keyed by delegated account pubkey.
    async fn fetch_delegation_metadata(
        &self,
        accounts: &[AccountSnapshot],
        min_context_slot: u64,
    ) -> TaskInfoProviderResult<HashMap<Pubkey, DelegationMetadata>>;

    async fn get_base_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoProviderResult<HashMap<Pubkey, Account>>;
}

// ---------------------------------------------------------------------------
// RpcTaskInfoProvider
// ---------------------------------------------------------------------------

/// Pure RPC implementation of [`TaskInfoProvider`] — no caching.
pub(crate) struct RpcTaskInfoProvider {
    rpc_client: MagicblockRpcClient,
}

impl RpcTaskInfoProvider {
    pub(crate) fn new(rpc_client: MagicblockRpcClient) -> Self {
        Self { rpc_client }
    }

    /// Fetches [`DelegationMetadata`]s with some num of retries
    pub(crate) async fn fetch_metadata_with_retries(
        rpc_client: &MagicblockRpcClient,
        accounts: &[AccountSnapshot],
        min_context_slot: u64,
        max_retries: NonZeroUsize,
    ) -> TaskInfoProviderResult<Vec<DelegationMetadata>> {
        if accounts.is_empty() {
            return Ok(Vec::new());
        }

        // Fetch the delegation record alongside the metadata to pin the
        // delegation session: a record created after the account's
        // snapshot slot belongs to a newer delegation of the same
        // account, and committing the old snapshot into it must fail
        // terminally.
        let pda_accounts: Vec<Pubkey> = accounts
            .iter()
            .flat_map(|(delegated_account, _)| {
                [
                    Pubkey::find_program_address(
                        delegation_record_seeds_from_delegated_account!(delegated_account),
                        &delegation_program_api::id(),
                    )
                    .0,
                    Pubkey::find_program_address(
                        delegation_metadata_seeds_from_delegated_account!(delegated_account),
                        &delegation_program_api::id(),
                    )
                    .0,
                ]
            })
            .collect();

        let fetched = Self::fetch_accounts_with_retries(
            rpc_client,
            &pda_accounts,
            min_context_slot,
            max_retries,
        )
        .await?;

        fetched
            .chunks_exact(2)
            .zip(pda_accounts.chunks_exact(2))
            .zip(accounts)
            .map(|((pair, pdas), (delegated_account, snapshot_slot))| {
                if pair.iter().any(|account| account.owner != dpi::id()) {
                    return Err(TaskInfoProviderError::InvalidAccountDataError(pdas[0]));
                }
                let record_data = pair[0]
                    .data
                    .get(..DelegationRecord::size_with_discriminator())
                    .ok_or(TaskInfoProviderError::InvalidAccountDataError(pdas[0]))?;
                let record = DelegationRecord::try_from_bytes_with_discriminator(record_data)
                    .map_err(|_| TaskInfoProviderError::InvalidAccountDataError(pdas[0]))?;
                // Snapshot slot 0 means the intent carries no snapshot
                // provenance (synthetic/test intents); skip the session
                // comparison since there is nothing to compare against.
                if *snapshot_slot > 0 && record.delegation_slot != *snapshot_slot {
                    return Err(TaskInfoProviderError::DelegationSessionChangedError {
                        delegated_account: *delegated_account,
                        delegation_slot: record.delegation_slot,
                        snapshot_slot: *snapshot_slot,
                    });
                }
                DelegationMetadata::try_from_bytes_with_discriminator(&pair[1].data)
                    .map_err(|_| TaskInfoProviderError::InvalidAccountDataError(pdas[1]))
            })
            .collect()
    }

    /// Fetches [`Account`]s with some num of retries
    pub(crate) async fn fetch_accounts_with_retries(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
        max_retries: NonZeroUsize,
    ) -> TaskInfoProviderResult<Vec<Account>> {
        if pubkeys.is_empty() {
            return Ok(Vec::new());
        }

        let mut i = 0;
        loop {
            i += 1;
            let err = match Self::fetch_accounts(rpc_client, pubkeys, min_context_slot).await {
                Ok(value) => break Ok(value),
                Err(err) => err,
            };

            match err {
                // A load-balanced RPC can briefly miss a newly created account.
                TaskInfoProviderError::AccountNotFoundError(ref pubkey) => {
                    warn!(
                        %pubkey,
                        min_context_slot,
                        attempt = i,
                        "Account not found, possibly stale RPC read"
                    );
                }
                err @ (TaskInfoProviderError::InvalidAccountDataError(_)
                | TaskInfoProviderError::DelegationSessionChangedError { .. }) => {
                    error!(error = ?err, "Unexpected error");
                    break Err(err);
                }
                TaskInfoProviderError::MinContextSlotNotReachedError(_, _) => {
                    info!(
                        min_context_slot,
                        attempt = i,
                        "Min context slot not reached"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                TaskInfoProviderError::MagicBlockRpcClientError(ref err) => {
                    warn!(error = ?err, attempt = i, "Fetch account error");
                }
            }

            if i >= max_retries.get() {
                break Err(err);
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Fetches specified list of accounts
    pub(crate) async fn fetch_accounts(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoProviderResult<Vec<Account>> {
        if pubkeys.is_empty() {
            return Ok(Vec::new());
        }

        metrics::inc_task_info_fetcher_a_count();
        let commitment = rpc_client.commitment();
        let mut accounts = rpc_client
            .get_multiple_accounts_with_config(
                pubkeys,
                RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64Zstd),
                    commitment: Some(commitment),
                    data_slice: None,
                    min_context_slot: Some(min_context_slot),
                },
                None,
            )
            .await
            .map_err(|err| TaskInfoProviderError::map_client_error(min_context_slot, err))?;

        let accounts = pubkeys
            .iter()
            .enumerate()
            .map(|(i, pubkey)| {
                let account = if let Some(account) = accounts.get_mut(i) {
                    account
                } else {
                    return Err(TaskInfoProviderError::AccountNotFoundError(*pubkey));
                };
                if let Some(account) = account.take() {
                    Ok(account)
                } else {
                    Err(TaskInfoProviderError::AccountNotFoundError(*pubkey))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(accounts)
    }
}

#[async_trait]
impl TaskInfoProvider for RpcTaskInfoProvider {
    async fn fetch_delegation_metadata(
        &self,
        accounts: &[AccountSnapshot],
        min_context_slot: u64,
    ) -> TaskInfoProviderResult<HashMap<Pubkey, DelegationMetadata>> {
        Ok(accounts
            .iter()
            .map(|(pk, _)| *pk)
            .zip(
                Self::fetch_metadata_with_retries(
                    &self.rpc_client,
                    accounts,
                    min_context_slot,
                    NUM_FETCH_RETRIES,
                )
                .await?,
            )
            .collect())
    }

    async fn get_base_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoProviderResult<HashMap<Pubkey, Account>> {
        let accounts = Self::fetch_accounts_with_retries(
            &self.rpc_client,
            pubkeys,
            min_context_slot,
            NUM_FETCH_RETRIES,
        )
        .await?;
        Ok(pubkeys.iter().copied().zip(accounts).collect())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TaskInfoProviderError {
    #[error("Metadata not found for: {0}")]
    AccountNotFoundError(Pubkey),
    #[error("InvalidAccountDataError for: {0}")]
    InvalidAccountDataError(Pubkey),
    #[error(
        "Delegation session changed for {delegated_account}: delegated at slot {delegation_slot}, committed state is from slot {snapshot_slot}"
    )]
    DelegationSessionChangedError {
        delegated_account: Pubkey,
        delegation_slot: u64,
        snapshot_slot: u64,
    },
    #[error("Minimum context slot {0} not reached: {1}")]
    MinContextSlotNotReachedError(u64, Box<MagicBlockRpcClientError>),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(Box<MagicBlockRpcClientError>),
}

impl TaskInfoProviderError {
    /// RPC-side fetch failures are transient; malformed delegation
    /// accounts are deterministic. Not-found is transient too: the
    /// accounts fetched here exist from the ER's perspective, so it is
    /// most likely a lagging RPC node. Retries are session-guarded — a
    /// delegation created after the committed snapshot fails terminally
    /// via `DelegationSessionChangedError`, as does a genuinely absent
    /// account once the bounded intent-level retries are exhausted.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::AccountNotFoundError(_) => true,
            Self::MinContextSlotNotReachedError(_, _) => true,
            Self::MagicBlockRpcClientError(err) => err.is_transient(),
            Self::InvalidAccountDataError(_) => false,
            Self::DelegationSessionChangedError { .. } => false,
        }
    }

    pub fn map_client_error(min_context_slot: u64, e: MagicBlockRpcClientError) -> Self {
        const MIN_CONTEXT_SLOT_MSG1: &str = "Minimum context slot has not been reached";
        const MIN_CONTEXT_SLOT_MSG2: &str = "Min context slot not reached";
        fn is_min_context_slot_msg(msg: &str) -> bool {
            msg.contains(MIN_CONTEXT_SLOT_MSG1) || msg.contains(MIN_CONTEXT_SLOT_MSG2)
        }

        let orig = e;
        let err = match &orig {
            MagicBlockRpcClientError::RpcClientError(err)
            | MagicBlockRpcClientError::SendTransaction(err) => Some(err),
            _ => None,
        };
        let Some(err) = err else {
            return Self::MagicBlockRpcClientError(Box::new(orig));
        };

        match &*err.kind {
            ErrorKind::RpcError(rpc_err) => match rpc_err {
                RpcError::ForUser(msg) if is_min_context_slot_msg(msg) => {
                    Self::MinContextSlotNotReachedError(min_context_slot, Box::new(orig))
                }
                RpcError::RpcResponseError {
                    code: JSON_RPC_SERVER_ERROR_MIN_CONTEXT_SLOT_NOT_REACHED,
                    ..
                } => Self::MinContextSlotNotReachedError(min_context_slot, Box::new(orig)),
                RpcError::RpcResponseError { message, .. } if is_min_context_slot_msg(message) => {
                    Self::MinContextSlotNotReachedError(min_context_slot, Box::new(orig))
                }
                _ => Self::MagicBlockRpcClientError(Box::new(orig)),
            },
            _ => Self::MagicBlockRpcClientError(Box::new(orig)),
        }
    }
}

impl From<MagicBlockRpcClientError> for TaskInfoProviderError {
    fn from(e: MagicBlockRpcClientError) -> Self {
        Self::MagicBlockRpcClientError(Box::new(e))
    }
}

impl TaskInfoProviderError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::AccountNotFoundError(_) => None,
            Self::InvalidAccountDataError(_) => None,
            Self::DelegationSessionChangedError { .. } => None,
            Self::MinContextSlotNotReachedError(_, err) => err.signature(),
            Self::MagicBlockRpcClientError(err) => err.signature(),
        }
    }
}

pub type TaskInfoProviderResult<T, E = TaskInfoProviderError> = Result<T, E>;
