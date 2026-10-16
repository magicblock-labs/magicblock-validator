use chainsync::{AccountProperty, ChainSync, ChainSyncAccount, Result as ChainSyncResult};
use engine::Engine;
use json::{Deserialize, Serialize};
use nucleus::runtime::TransactionView;
use serde::{Deserializer, de::Error as _};
use solana_account::{AccountMode, AccountSharedData};
use solana_account_decoder::{UiAccount, UiAccountEncoding, UiDataSliceConfig, encode_ui_account};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::RpcProgramAccountsConfig,
    filter::{Memcmp, MemcmpEncodedBytes, RpcFilterError, RpcFilterType},
};
use solana_transaction_status::UiTransactionEncoding;
use spl_token_2022::{generic_token_account::GenericTokenAccount, state::Account as TokenAccount};
use std::sync::Arc;

use crate::{
    RpcResult, error::RpcError, requests::params::Serde32Bytes, transaction::decode_transaction,
};
use tracing::warn;

/// Account synchronization and scoped reads used by fetch-capable RPCs.
pub(crate) struct Accounts {
    engine: Engine,
    chainsync: Arc<ChainSync>,
}

impl Accounts {
    pub(crate) fn new(engine: Engine, chainsync: Arc<ChainSync>) -> Self {
        Self { engine, chainsync }
    }

    /// Uninitialized images are storage placeholders, not RPC-visible accounts.
    pub(crate) fn account_is_visible(account: &AccountSharedData) -> bool {
        !account.is(AccountMode::Uninit)
    }

    /// Ensure best-effort, then project the account while its borrowed image is scoped.
    pub(crate) async fn read<R>(
        &self,
        pubkey: &Pubkey,
        fetches: &mut u64,
        reader: impl Fn(&AccountSharedData) -> R,
    ) -> Option<R> {
        // Single-account RPCs tolerate ensure failures and read any existing image.
        let _ = self.fetch(&[*pubkey], fetches).await;
        self.engine.accounts().loader().read(pubkey, reader).ok().flatten()
    }

    /// Read projections in request order after best-effort synchronization.
    pub(crate) async fn read_many<R>(
        &self,
        pubkeys: &[Pubkey],
        fetches: &mut u64,
        reader: impl Fn(&Pubkey, &AccountSharedData) -> R,
    ) -> Vec<Option<R>> {
        self.ensure(pubkeys, fetches).await;
        let accessor = self.engine.accounts();
        let loader = accessor.loader();
        pubkeys
            .iter()
            .map(|pubkey| loader.read(pubkey, |account| reader(pubkey, account)).ok().flatten())
            .collect()
    }

    /// Decode and synchronize transaction keys; unlike account reads, fetch errors reject admission.
    pub(crate) async fn prepare_transaction(
        &self,
        transaction: &str,
        encoding: UiTransactionEncoding,
        fetches: &mut u64,
    ) -> RpcResult<TransactionView> {
        let transaction = decode_transaction(transaction, encoding)?;
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
        let requests = pubkeys.iter().map(|&pubkey| ChainSyncAccount {
            pubkey,
            property: AccountProperty::Readonly,
        });
        *fetches += self.chainsync.ensure(requests).await?;
        Ok(())
    }
}

/// A keyed RPC account whose pubkey uses the allocation-free Base58 serializer.
#[derive(Serialize)]
pub(crate) struct AccountWithPubkey {
    pubkey: Serde32Bytes,
    account: UiAccount,
}

impl AccountWithPubkey {
    pub(crate) fn new(
        pubkey: Pubkey,
        account: &AccountSharedData,
        encoding: UiAccountEncoding,
        slice: Option<UiDataSliceConfig>,
    ) -> Self {
        Self {
            pubkey: pubkey.into(),
            account: encode_ui_account(&pubkey, account, encoding, None, slice),
        }
    }
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

impl PreparedFilter {
    /// Matches account data without repeating memcmp decoding.
    pub(crate) fn matches(&self, data: &[u8]) -> bool {
        match &self.0 {
            RpcFilterType::DataSize(size) => data.len() as u64 == *size,
            RpcFilterType::Memcmp(memcmp) => memcmp.bytes_match(data),
            RpcFilterType::TokenAccountState => TokenAccount::valid_account_data(data),
        }
    }
}

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
                // Bound decoding work using Solana's encoded limits. The upstream
                // verify below enforces the 128-byte limit for all decoded/raw inputs.
                let oversized = match &compare.bytes {
                    MemcmpEncodedBytes::Base58(bytes) => bytes.len() > 175,
                    MemcmpEncodedBytes::Base64(bytes) => bytes.len() > 172,
                    MemcmpEncodedBytes::Bytes(_) => false,
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
