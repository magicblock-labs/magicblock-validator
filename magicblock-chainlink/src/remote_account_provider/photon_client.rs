use std::{ops::Deref, sync::Arc};

use async_trait::async_trait;
use light_client::indexer::{
    photon_indexer::PhotonIndexer, CompressedAccount, Context, Indexer,
    IndexerRpcConfig, Response,
};
use magicblock_core::compression::derive_cda_from_pda;
use solana_account::Account;
use solana_clock::Slot;
use solana_pubkey::Pubkey;
use thiserror::Error;
use tracing::*;

fn install_default_rustls_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

#[derive(Debug, Clone, Error)]
pub enum PhotonClientError {
    #[error("Indexer error: {0}")]
    IndexerError(#[from] light_client::indexer::IndexerError),
}

pub type PhotonClientResult<T> = Result<T, PhotonClientError>;

#[async_trait]
pub trait PhotonClient: Send + Sync + Clone + 'static {
    async fn get_account(
        &self,
        pubkey: &Pubkey,
        min_context_slot: Option<Slot>,
    ) -> PhotonClientResult<Option<(Account, Slot)>>;

    async fn get_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: Option<Slot>,
    ) -> PhotonClientResult<(Vec<Option<Account>>, Slot)>;
}

#[derive(Clone)]
pub struct PhotonClientImpl(Arc<PhotonIndexer>);

impl Deref for PhotonClientImpl {
    type Target = Arc<PhotonIndexer>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PhotonClientImpl {
    pub fn new(photon_indexer: Arc<PhotonIndexer>) -> Self {
        install_default_rustls_crypto_provider();
        Self(photon_indexer)
    }
    pub fn new_from_url(url: String) -> Self {
        Self::new(Arc::new(PhotonIndexer::new(url)))
    }
}

#[async_trait]
impl PhotonClient for PhotonClientImpl {
    async fn get_account(
        &self,
        pubkey: &Pubkey,
        min_context_slot: Option<Slot>,
    ) -> PhotonClientResult<Option<(Account, Slot)>> {
        let config = min_context_slot.map(|slot| IndexerRpcConfig {
            slot,
            ..Default::default()
        });
        let cda = derive_cda_from_pda(pubkey);
        let Response {
            value: compressed_acc,
            context: Context { slot, .. },
        } = self.get_compressed_account(cda.to_bytes(), config).await?;
        let account = account_from_compressed_account(compressed_acc);
        Ok(account.map(|acc| (acc, slot)))
    }

    async fn get_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: Option<Slot>,
    ) -> PhotonClientResult<(Vec<Option<Account>>, Slot)> {
        let config = min_context_slot.map(|slot| IndexerRpcConfig {
            slot,
            ..Default::default()
        });
        let cdas: Vec<_> = pubkeys
            .iter()
            .map(|pk| derive_cda_from_pda(pk).to_bytes())
            .collect();

        if tracing::enabled!(tracing::Level::DEBUG) {
            let pks_cdas = pubkeys
                .iter()
                .zip(cdas.iter())
                .map(|(pk, cda)| {
                    format!("({}: {})", pk, Pubkey::new_from_array(*cda))
                })
                .collect::<Vec<_>>()
                .join(", ");
            debug!(cdas = %pks_cdas, "Fetching multiple accounts");
        }

        let Response {
            value: compressed_accs,
            context: Context { slot, .. },
        } = self
            .get_multiple_compressed_accounts(Some(cdas), None, config)
            .await?;

        let accounts = compressed_accs
            .items
            .into_iter()
            .map(account_from_compressed_account)
            .collect();
        Ok((accounts, slot))
    }
}

// -----------------
// Helpers
// -----------------

fn account_from_compressed_account(
    compressed_acc: Option<CompressedAccount>,
) -> Option<Account> {
    let compressed_acc = compressed_acc?;
    let data = compressed_acc.data?.data;
    // NOTE: delegated compressed accounts are set to zero lamports when cloned
    // Actual lamports have to be paid back when undelegating
    Some(Account {
        lamports: 0,
        data,
        owner: compressed_acc.owner.to_bytes().into(),
        executable: false,
        rent_epoch: 0,
    })
}
