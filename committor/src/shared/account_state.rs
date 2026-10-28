use std::{collections::HashMap, num::NonZeroUsize, time::Duration};

use async_trait::async_trait;
use delegation_program_interface::{
    self as dpi, DelegationCluster, DelegationState, UndelegationRequester,
    legacy::state::DelegationRecord,
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
use wheels::layout::Decodable;

const NUM_FETCH_RETRIES: NonZeroUsize = NonZeroUsize::new(5).unwrap();

/// A delegated account paired with the base-layer slot of the state
/// snapshot being committed for it (`CommittedAccount::remote_slot`).
/// The slot pins the delegation session: a delegation record created
/// after it belongs to a newer delegation of the same account.
pub type AccountSnapshot = (Pubkey, u64);

/// The on-chain delegation format determines the settlement instructions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum DelegationVersion {
    #[default]
    Legacy,
    V2 {
        cluster: Pubkey,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct DelegationInfo {
    pub last_commit_id: u64,
    pub undelegation_requester: UndelegationRequester,
    pub rent_payer: Pubkey,
    pub version: DelegationVersion,
}

#[async_trait]
pub(crate) trait TaskInfoProvider: Send + Sync + 'static {
    /// Fetches delegation metadata keyed by delegated account pubkey.
    async fn fetch_delegation_info(
        &self,
        accounts: &[AccountSnapshot],
        min_context_slot: u64,
    ) -> TaskInfoProviderResult<HashMap<Pubkey, DelegationInfo>>;

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

    /// Reads either layout, keeping each account's PDAs in one RPC snapshot.
    pub(crate) async fn fetch_metadata_with_retries(
        rpc_client: &MagicblockRpcClient,
        accounts: &[AccountSnapshot],
        min_context_slot: u64,
        max_retries: NonZeroUsize,
    ) -> TaskInfoProviderResult<Vec<DelegationInfo>> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match Self::fetch_metadata(rpc_client, accounts, min_context_slot).await {
                Ok(metadata) => return Ok(metadata),
                Err(err) if attempt == max_retries.get() || !err.is_transient() => return Err(err),
                Err(err) => warn!(attempt, error = ?err, "Failed to read delegation metadata"),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn fetch_metadata(
        rpc_client: &MagicblockRpcClient,
        accounts: &[AccountSnapshot],
        min_context_slot: u64,
    ) -> TaskInfoProviderResult<Vec<DelegationInfo>> {
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let pdas: Vec<_> = accounts
            .iter()
            .flat_map(|(account, _)| {
                [
                    dpi::legacy::pda::delegation_record_pda_from_delegated_account(account),
                    dpi::legacy::pda::delegation_info_pda_from_delegated_account(account),
                    dpi::delegation_state_pda(account),
                ]
            })
            .collect();
        // 99 keeps all three PDAs in the same getMultipleAccounts response.
        let fetched =
            Self::fetch_optional_accounts(rpc_client, &pdas, min_context_slot, Some(99)).await?;
        let mut metadata = Vec::with_capacity(accounts.len());
        let mut clusters = Vec::new();
        let mut cluster_min_slot = min_context_slot;
        for ((triple, pdas), &(account, snapshot_slot)) in
            fetched.chunks_exact(3).zip(pdas.chunks_exact(3)).zip(accounts)
        {
            let (delegation_slot, value) = parse_delegation_info(account, triple, pdas)?;
            if snapshot_slot > 0 && delegation_slot > snapshot_slot {
                return Err(TaskInfoProviderError::DelegationSessionChangedError {
                    delegated_account: account,
                    delegation_slot,
                    snapshot_slot,
                });
            }
            if let DelegationVersion::V2 { cluster } = value.version {
                clusters.push(cluster);
                cluster_min_slot = cluster_min_slot.max(delegation_slot);
            }
            metadata.push(value);
        }
        clusters.sort_unstable();
        clusters.dedup();
        let cluster_accounts =
            Self::fetch_optional_accounts(rpc_client, &clusters, cluster_min_slot, None).await?;
        for (pubkey, account) in clusters.into_iter().zip(cluster_accounts) {
            let account = account.ok_or(TaskInfoProviderError::AccountNotFoundError(pubkey))?;
            validate_delegation_owner(pubkey, &account)?;
            let cluster = DelegationCluster::decode(&account.data)
                .map_err(|_| TaskInfoProviderError::InvalidAccountDataError(pubkey))?;
            if cluster.discriminator() != DelegationCluster::DISCRIMINATOR
                || dpi::delegation_cluster_pda(cluster.cluster_id()) != pubkey
                || cluster.challenge_config().is_some()
            {
                return Err(TaskInfoProviderError::InvalidAccountDataError(pubkey));
            }
        }
        Ok(metadata)
    }

    async fn fetch_optional_accounts(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
        max_per_fetch: Option<usize>,
    ) -> TaskInfoProviderResult<Vec<Option<Account>>> {
        if pubkeys.is_empty() {
            return Ok(Vec::new());
        }
        metrics::inc_task_info_fetcher_a_count();
        let accounts = rpc_client
            .get_multiple_accounts_with_config(
                pubkeys,
                RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64Zstd),
                    commitment: Some(rpc_client.commitment()),
                    min_context_slot: Some(min_context_slot),
                    ..Default::default()
                },
                max_per_fetch,
            )
            .await
            .map_err(|err| TaskInfoProviderError::map_client_error(min_context_slot, err))?;
        if accounts.len() != pubkeys.len() {
            return Err(TaskInfoProviderError::AccountNotFoundError(pubkeys[0]));
        }
        Ok(accounts)
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

        let mut accounts =
            Self::fetch_optional_accounts(rpc_client, pubkeys, min_context_slot, None).await?;

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

fn validate_delegation_owner(pubkey: Pubkey, account: &Account) -> TaskInfoProviderResult<()> {
    if account.owner != dpi::id() || account.executable {
        return Err(TaskInfoProviderError::InvalidAccountDataError(pubkey));
    }
    Ok(())
}

fn parse_delegation_info(
    delegated_account: Pubkey,
    accounts: &[Option<Account>],
    pdas: &[Pubkey],
) -> TaskInfoProviderResult<(u64, DelegationInfo)> {
    match (&accounts[0], &accounts[1], &accounts[2]) {
        (Some(record), Some(metadata), None) => {
            validate_delegation_owner(pdas[0], record)?;
            validate_delegation_owner(pdas[1], metadata)?;
            let record = DelegationRecord::try_from_bytes_with_discriminator(&record.data)
                .map_err(|_| TaskInfoProviderError::InvalidAccountDataError(pdas[0]))?;
            let metadata =
                dpi::legacy::state::DelegationMetadata::try_from_bytes_with_discriminator(
                    &metadata.data,
                )
                .map_err(|_| TaskInfoProviderError::InvalidAccountDataError(pdas[1]))?;
            let requester = match metadata.undelegation_requester {
                dpi::legacy::state::UndelegationRequester::None => UndelegationRequester::None,
                dpi::legacy::state::UndelegationRequester::Validator => {
                    UndelegationRequester::Validator
                }
                dpi::legacy::state::UndelegationRequester::OwnerProgram => {
                    UndelegationRequester::OwnerProgram
                }
            };
            Ok((
                record.delegation_slot,
                DelegationInfo {
                    last_commit_id: metadata.last_commit_id,
                    undelegation_requester: requester,
                    rent_payer: metadata.rent_payer,
                    version: DelegationVersion::Legacy,
                },
            ))
        }
        (None, None, Some(state_account)) => {
            validate_delegation_owner(pdas[2], state_account)?;
            let state = DelegationState::decode(&state_account.data)
                .map_err(|_| TaskInfoProviderError::InvalidAccountDataError(pdas[2]))?;
            if state.discriminator() != DelegationState::DISCRIMINATOR
                || *state.delegated_account() != delegated_account
            {
                return Err(TaskInfoProviderError::InvalidAccountDataError(pdas[2]));
            }
            let requester = match state.undelegation_requester() {
                0 => UndelegationRequester::None,
                1 => UndelegationRequester::Validator,
                2 => UndelegationRequester::OwnerProgram,
                _ => return Err(TaskInfoProviderError::InvalidAccountDataError(pdas[2])),
            };
            Ok((
                state.delegation_slot(),
                DelegationInfo {
                    last_commit_id: state.last_commit_id(),
                    undelegation_requester: requester,
                    rent_payer: *state.rent_payer(),
                    version: DelegationVersion::V2 {
                        cluster: *state.delegation_cluster(),
                    },
                },
            ))
        }
        // Never silently pick one protocol when both layouts are present.
        (_, _, Some(_)) => Err(TaskInfoProviderError::InvalidAccountDataError(pdas[2])),
        (None, _, None) => Err(TaskInfoProviderError::AccountNotFoundError(pdas[0])),
        (_, None, None) => Err(TaskInfoProviderError::AccountNotFoundError(pdas[1])),
    }
}

#[async_trait]
impl TaskInfoProvider for RpcTaskInfoProvider {
    async fn fetch_delegation_info(
        &self,
        accounts: &[AccountSnapshot],
        min_context_slot: u64,
    ) -> TaskInfoProviderResult<HashMap<Pubkey, DelegationInfo>> {
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
