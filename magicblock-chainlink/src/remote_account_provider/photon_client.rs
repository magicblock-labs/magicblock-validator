use std::{ops::Deref, sync::Arc};

use light_client::indexer::{
    photon_indexer::PhotonIndexer, Context, Indexer, IndexerError,
    IndexerRpcConfig, Response,
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
    pub fn new(photon_indexer: Arc<PhotonIndexer>) -> Self {
        Self(photon_indexer)
    }
    pub fn new_from_url(url: &str) -> Self {
        Self::new(Arc::new(PhotonIndexer::new(url.to_string(), None)))
    }

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
            Err(err) if matches!(err, IndexerError::AccountNotFound) => {
                return Ok(None);
            }
            Err(err) => {
                return Err(err.into());
            }
        };
        let data = compressed_acc.data.unwrap_or_default().data;
        let account = Account {
            lamports: compressed_acc.lamports,
            data,
            owner: compressed_acc.owner,
            executable: false,
            rent_epoch: 0,
        };
        Ok(Some((account, slot)))
    }
}
