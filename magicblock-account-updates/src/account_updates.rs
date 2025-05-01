use solana_sdk::{clock::Slot, pubkey::Pubkey};
use thiserror::Error;
use tokio::sync::mpsc::error::SendError;

#[derive(Debug, Clone, Error)]
pub enum AccountUpdatesError {
    #[error(transparent)]
    SendError(#[from] SendError<(Pubkey, bool)>),
}

pub type AccountUpdatesResult<T> = Result<T, AccountUpdatesError>;

pub trait AccountUpdates {
    #[allow(async_fn_in_trait)]
    async fn ensure_account_monitoring(
        &self,
        pubkey: &Pubkey,
    ) -> AccountUpdatesResult<()>;
    #[allow(async_fn_in_trait)]
    async fn stop_account_monitoring(
        &self,
        _pubkey: &Pubkey,
    ) -> AccountUpdatesResult<()> {
        Ok(())
    }
    fn get_first_subscribed_slot(&self, pubkey: &Pubkey) -> Option<Slot>;
    fn get_last_known_update_slot(&self, pubkey: &Pubkey) -> Option<Slot>;
}
