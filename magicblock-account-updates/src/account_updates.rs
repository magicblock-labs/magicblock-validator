use solana_sdk::{clock::Slot, pubkey::Pubkey};
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum AccountUpdatesError {
    #[error(transparent)]
    SendError(#[from] tokio::sync::mpsc::error::SendError<Pubkey>),
}

pub type AccountUpdatesResult<T> = Result<T, AccountUpdatesError>;

pub trait AccountUpdates {
    #[allow(async_fn_in_trait)]
    async fn ensure_account_monitoring(
        &self,
        pubkey: &Pubkey,
    ) -> AccountUpdatesResult<()>;
    fn get_first_subscribed_slot(&self, pubkey: &Pubkey) -> Option<Slot>;
    fn get_last_known_update_slot(&self, pubkey: &Pubkey) -> Option<Slot>;
}
