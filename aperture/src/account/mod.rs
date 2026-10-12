use engine::Engine;
use chainsync::{AccountProperty, ChainSync, ChainSyncAccount, Result as ChainSyncResult};
use metrics::metrics::ENSURE_ACCOUNTS_TIME;
use nucleus::runtime::TransactionView;
use serde::{Deserialize, Deserializer, de::Error as _};
use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::RpcProgramAccountsConfig,
    filter::{Memcmp, MemcmpEncodedBytes, RpcFilterError, RpcFilterType},
};
use solana_transaction_status::UiTransactionEncoding;
use spl_token_2022::{generic_token_account::GenericTokenAccount, state::Account as TokenAccount};
use std::sync::Arc;

use crate::{RpcResult, error::RpcError, transaction::decode_transaction};
use tracing::warn;

pub(crate) mod encoding;

/// Account synchronization and scoped reads used by fetch-capable RPCs.
pub(crate) struct Accounts {
    engine: Engine,
    chainsync: Arc<ChainSync>,
}

impl Accounts {
    pub(crate) fn new(engine: Engine, chainsync: Arc<ChainSync>) -> Self {
        Self { engine, chainsync }
    }

    /// Ensure best-effort, then project the account while its borrowed image is scoped.
    pub(crate) async fn read<R>(
        &self,
        pubkey: &Pubkey,
        fetches: &mut u64,
        reader: impl Fn(&AccountSharedData) -> R,
    ) -> Option<R> {
        self.ensure_one(pubkey, fetches).await;
        self.engine.accounts().loader().read(pubkey, reader).ok().flatten()
    }

    /// Single-account RPCs tolerate ensure failures and read any existing image.
    pub(crate) async fn ensure_one(
        &self,
        pubkey: &Pubkey,
        fetches: &mut u64,
    ) {
        let _ = self.fetch(&[*pubkey], fetches).await;
    }

    /// Decode and synchronize transaction keys; unlike account reads, fetch errors reject admission.
    pub(crate) async fn prepare_transaction(
        &self,
        transaction: &str,
        encoding: UiTransactionEncoding,
        fetches: &mut u64,
    ) -> RpcResult<TransactionView> {
        let transaction = decode_transaction(transaction, encoding)?;
        let _timer = ENSURE_ACCOUNTS_TIME.with_label_values(&["transaction"]).start_timer();
        *fetches += self
            .chainsync
            .ensure_transaction_accounts(&transaction)
            .await
            .inspect_err(|error| warn!(?error, "failed to ensure transaction accounts"))
            .map_err(RpcError::transaction_verification)?;
        Ok(transaction)
    }

    /// Multi-account RPCs log ensure failures but continue with locally available images.
    pub(crate) async fn ensure(&self, pubkeys: &[Pubkey], fetches: &mut u64) {
        let _ = self
            .fetch(pubkeys, fetches)
            .await
            .inspect_err(|error| warn!(?error, "failed to ensure accounts"));
    }

    /// Account for successful synchronization before any later projection or admission error.
    async fn fetch(&self, pubkeys: &[Pubkey], fetches: &mut u64) -> ChainSyncResult<()> {
        let _timer = ENSURE_ACCOUNTS_TIME.with_label_values(&["account"]).start_timer();
        let requests = pubkeys.iter().map(|&pubkey| ChainSyncAccount {
            pubkey,
            property: AccountProperty::Readonly,
        });
        *fetches += self.chainsync.ensure(requests).await?;
        Ok(())
    }
}

/// Match validated filters without decoding their bytes for each account.
pub(crate) fn matches_filters(filters: &[PreparedFilter], data: &[u8]) -> bool {
    filters.iter().all(|filter| match &filter.0 {
        RpcFilterType::DataSize(size) => data.len() as u64 == *size,
        RpcFilterType::Memcmp(memcmp) => memcmp.bytes_match(data),
        RpcFilterType::TokenAccountState => TokenAccount::valid_account_data(data),
    })
}

/// The upstream program config, with filters decoded during parameter parsing.
#[derive(Deserialize, Default)]
pub(crate) struct ProgramConfig {
    #[serde(flatten)]
    pub(crate) config: RpcProgramAccountsConfig,
    pub(crate) filters: Option<Vec<PreparedFilter>>,
}

/// A size-checked RPC filter with memcmp bytes decoded once, before scanning or subscribing.
pub(crate) struct PreparedFilter(RpcFilterType);

impl<'de> Deserialize<'de> for PreparedFilter {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// Wire variants whose memcmp bytes have not yet been decoded or validated.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        enum Filter {
            DataSize(u64),
            Memcmp(Compare),
            TokenAccountState,
        }
        /// Keep Solana's bytes codec so default and explicit encodings retain their semantics.
        #[derive(Deserialize)]
        struct Compare {
            offset: usize,
            #[serde(flatten)]
            bytes: MemcmpEncodedBytes,
        }

        let filter = match Filter::deserialize(deserializer)? {
            Filter::DataSize(size) => RpcFilterType::DataSize(size),
            Filter::TokenAccountState => RpcFilterType::TokenAccountState,
            Filter::Memcmp(compare) => {
                // Reject oversized wire values before allocating decoded bytes. These
                // are Solana's encoded limits; verify below enforces the raw 128-byte limit.
                let oversized = match &compare.bytes {
                    MemcmpEncodedBytes::Base58(bytes) => bytes.len() > 175,
                    MemcmpEncodedBytes::Base64(bytes) => bytes.len() > 172,
                    MemcmpEncodedBytes::Bytes(bytes) => bytes.len() > 128,
                };
                if oversized {
                    return Err(D::Error::custom(RpcFilterError::DataTooLarge));
                }
                let mut memcmp = Memcmp::new(compare.offset, compare.bytes);
                memcmp.convert_to_raw_bytes().map_err(D::Error::custom)?;
                RpcFilterType::Memcmp(memcmp)
            }
        };
        filter.verify().map_err(D::Error::custom)?;
        Ok(Self(filter))
    }
}
