use hyper::body::Bytes;
use mbv_core::Slot;
use serde::Serialize;
use solana_transaction_error::{TransactionError, TransactionResult};

use crate::{
    RpcResult, requests::payload::NotificationPayload, state::subscriptions::SubscriptionID,
};

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
