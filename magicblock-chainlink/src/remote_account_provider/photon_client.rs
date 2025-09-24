use std::{ops::Deref, sync::Arc};

use async_trait::async_trait;
use light_client::indexer::{
    photon_indexer::PhotonIndexer, CompressedAccount, Context, Indexer,
    IndexerError, IndexerRpcConfig, Response,
};
use magicblock_core::compression::derive_cda_from_pda;
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_sdk::clock::Slot;

use crate::remote_account_provider::RemoteAccountProviderResult;

#[derive(Clone)]
pub struct PhotonClientImpl(Arc<PhotonIndexer>);

impl Deref for PhotonClientImpl {
    type Target = Arc<PhotonIndexer>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PhotonClientImpl {
    pub(crate) fn new(photon_indexer: Arc<PhotonIndexer>) -> Self {
        Self(photon_indexer)
    }
    pub(crate) fn new_from_url(url: &str) -> Self {
        Self::new(Arc::new(PhotonIndexer::new(url.to_string(), None)))
    }
}

#[async_trait]
pub trait PhotonClient: Send + Sync + Clone + 'static {
    async fn get_account(
        &self,
        pubkey: &Pubkey,
        min_context_slot: Option<Slot>,
    ) -> RemoteAccountProviderResult<Option<(Account, Slot)>>;

    async fn get_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: Option<Slot>,
    ) -> RemoteAccountProviderResult<(Vec<Option<Account>>, Slot)>;
}

#[async_trait]
impl PhotonClient for PhotonClientImpl {
    async fn get_account(
        &self,
        pubkey: &Pubkey,
        min_context_slot: Option<Slot>,
    ) -> RemoteAccountProviderResult<Option<(Account, Slot)>> {
        let config = min_context_slot.map(|slot| IndexerRpcConfig {
            slot,
            ..Default::default()
        });
        let cda = derive_cda_from_pda(pubkey);
        let Response {
            value: compressed_acc,
            context: Context { slot, .. },
        } = match self.get_compressed_account(cda.to_bytes(), config).await {
            Ok(res) => res,
            Err(IndexerError::AccountNotFound) => {
                return Ok(None);
            }
            Err(err) => {
                return Err(err.into());
            }
        };
        let account = account_from_compressed_account(compressed_acc);
        Ok(Some((account, slot)))
    }

    async fn get_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: Option<Slot>,
    ) -> RemoteAccountProviderResult<(Vec<Option<Account>>, Slot)> {
        let config = min_context_slot.map(|slot| IndexerRpcConfig {
            slot,
            ..Default::default()
        });
        let cdas: Vec<_> = pubkeys
            .iter()
            .map(|pk| derive_cda_from_pda(pk).to_bytes())
            .collect();
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
            // NOTE: the light-client API is incorrect currently.
            // The server will return `None` for missing accounts,
            .map(Some)
            .collect();
        Ok((accounts, slot))
    }
}

// -----------------
// Helpers
// -----------------
fn account_from_compressed_account(
    compressed_acc: CompressedAccount,
) -> Account {
    let data = compressed_acc.data.unwrap_or_default().data;
    Account {
        lamports: compressed_acc.lamports,
        data,
        owner: compressed_acc.owner,
        executable: false,
        rent_epoch: 0,
    }
}
