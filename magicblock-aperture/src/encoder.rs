use std::fmt::Debug;

use hyper::body::Bytes;
use json::Serialize;
use magicblock_core::Slot;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_account_decoder::{
    UiAccountEncoding, UiDataSliceConfig, encode_ui_account,
};
use solana_pubkey::Pubkey;
use solana_transaction_error::{TransactionError, TransactionResult};

use crate::{
    requests::payload::NotificationPayload,
    state::subscriptions::SubscriptionID,
    utils::{AccountWithPubkey, ProgramFilters},
};

/// An abstraction trait over types which specialize in turning various
/// websocket notification payload types into sequence of bytes
pub(crate) trait Encoder: Ord + Eq + Clone + Debug {
    type Data;
    fn encode(
        &self,
        slot: Slot,
        data: &Self::Data,
        id: SubscriptionID,
    ) -> Option<Bytes>;
}

/// A `accountSubscribe` payload encoder
#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) struct AccountEncoder {
    pub(crate) encoding: UiAccountEncoding,
    pub(crate) data_slice: Option<UiDataSliceConfig>,
}

impl PartialOrd for AccountEncoder {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AccountEncoder {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let key = |e: &Self| {
            (
                e.encoding as u32,
                e.data_slice.map(|ds| (ds.offset, ds.length)),
            )
        };
        key(self).cmp(&key(other))
    }
}

/// A `programSubscribe` payload encoder
#[derive(PartialEq, PartialOrd, Ord, Eq, Clone, Debug)]
pub struct ProgramAccountEncoder {
    pub encoder: AccountEncoder,
    pub filters: ProgramFilters,
}

impl Encoder for AccountEncoder {
    type Data = (Pubkey, AccountSharedData);

    fn encode(
        &self,
        slot: Slot,
        data: &Self::Data,
        id: SubscriptionID,
    ) -> Option<Bytes> {
        let (pubkey, account) = data;
        let encoded = encode_ui_account(
            pubkey,
            account,
            self.encoding,
            None,
            self.data_slice,
        );
        let method = "accountNotification";
        NotificationPayload::encode(encoded, slot, method, id)
    }
}

impl Encoder for ProgramAccountEncoder {
    type Data = (Pubkey, AccountSharedData);

    fn encode(
        &self,
        slot: Slot,
        data: &Self::Data,
        id: SubscriptionID,
    ) -> Option<Bytes> {
        let (pubkey, account) = data;
        self.filters.matches(account.data()).then_some(())?;
        let value = AccountWithPubkey::new(
            *pubkey,
            account,
            self.encoder.encoding,
            self.encoder.data_slice,
        );
        let method = "programNotification";
        NotificationPayload::encode(value, slot, method, id)
    }
}

/// A `signatureSubscribe` payload encoder
#[derive(PartialEq, PartialOrd, Ord, Eq, Clone, Debug)]
pub(crate) struct TransactionResultEncoder;

impl Encoder for TransactionResultEncoder {
    type Data = TransactionResult<()>;

    fn encode(
        &self,
        slot: Slot,
        data: &Self::Data,
        id: SubscriptionID,
    ) -> Option<Bytes> {
        #[derive(Serialize)]
        struct SignatureResult {
            err: Option<TransactionError>,
        }
        let method = "signatureNotification";
        let err = data.as_ref().err().cloned();
        let result = SignatureResult { err };
        NotificationPayload::encode(result, slot, method, id)
    }
}

/// A `slotSubscribe` payload encoder
#[derive(PartialEq, PartialOrd, Ord, Eq, Clone, Debug)]
pub(crate) struct SlotEncoder;

impl Encoder for SlotEncoder {
    type Data = ();

    fn encode(
        &self,
        slot: Slot,
        _: &Self::Data,
        id: SubscriptionID,
    ) -> Option<Bytes> {
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
}
