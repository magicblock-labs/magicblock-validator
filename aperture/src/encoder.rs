use hyper::body::Bytes;
use json::Serialize;
use magicblock_core::Slot;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_account_decoder::{UiAccountEncoding, UiDataSliceConfig, encode_ui_account};
use solana_pubkey::Pubkey;
use solana_transaction_error::{TransactionError, TransactionResult};

use crate::{
    RpcResult,
    account::{AccountWithPubkey, PreparedFilter, matches_filters},
    requests::payload::NotificationPayload,
    state::subscriptions::SubscriptionID,
};

/// Account notification encoding options shared with program notifications.
pub(crate) struct AccountEncoder {
    pub(crate) encoding: UiAccountEncoding,
    pub(crate) data_slice: Option<UiDataSliceConfig>,
}

/// Prepared program filters applied before encoding an account notification.
pub(crate) struct ProgramAccountEncoder {
    pub(crate) encoder: AccountEncoder,
    pub(crate) filters: Vec<PreparedFilter>,
}

impl AccountEncoder {
    pub(crate) fn encode(
        &self,
        slot: Slot,
        pubkey: &Pubkey,
        account: &AccountSharedData,
        id: SubscriptionID,
    ) -> RpcResult<Bytes> {
        let encoded = encode_ui_account(pubkey, account, self.encoding, None, self.data_slice);
        let method = "accountNotification";
        NotificationPayload::encode(encoded, slot, method, id)
    }
}

impl ProgramAccountEncoder {
    pub(crate) fn encode(
        &self,
        slot: Slot,
        pubkey: &Pubkey,
        account: &AccountSharedData,
        id: SubscriptionID,
    ) -> RpcResult<Option<Bytes>> {
        if !matches_filters(&self.filters, account.data()) {
            return Ok(None);
        }
        let value = AccountWithPubkey::new(
            *pubkey,
            account,
            self.encoder.encoding,
            self.encoder.data_slice,
        );
        let method = "programNotification";
        NotificationPayload::encode(value, slot, method, id).map(Some)
    }
}

/// Encodes the terminal result of a one-shot signature subscription.
pub(crate) fn encode_signature(
    slot: Slot,
    data: &TransactionResult<()>,
    id: SubscriptionID,
) -> RpcResult<Bytes> {
    #[derive(Serialize)]
    struct SignatureResult {
        err: Option<TransactionError>,
    }
    let method = "signatureNotification";
    let err = data.as_ref().err().cloned();
    let result = SignatureResult { err };
    NotificationPayload::encode(result, slot, method, id)
}

/// Encodes the current slot with the RPC parent/root compatibility values.
pub(crate) fn encode_slot(slot: Slot, id: SubscriptionID) -> RpcResult<Bytes> {
    #[derive(Serialize)]
    struct SlotUpdate {
        slot: u64,
        parent: u64,
        root: u64,
    }
    let method = "slotNotification";
    let update = SlotUpdate {
        slot,
        parent: slot.saturating_sub(1),
        root: slot,
    };
    NotificationPayload::encode_no_context(update, method, id)
}
