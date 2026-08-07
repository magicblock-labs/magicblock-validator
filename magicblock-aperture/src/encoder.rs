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
    requests::{
        http::get_program_accounts::{AccountWithPubkey, matches_filters},
        payload::NotificationPayload,
    },
    state::subscriptions::SubscriptionID,
};

pub(crate) struct AccountEncoder {
    pub(crate) encoding: UiAccountEncoding,
    pub(crate) data_slice: Option<UiDataSliceConfig>,
}

pub(crate) struct ProgramAccountEncoder {
    pub(crate) encoder: AccountEncoder,
    pub(crate) filters: Vec<solana_rpc_client_api::filter::RpcFilterType>,
}

impl AccountEncoder {
    pub(crate) fn encode(
        &self,
        slot: Slot,
        data: &(Pubkey, AccountSharedData),
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

impl ProgramAccountEncoder {
    pub(crate) fn encode(
        &self,
        slot: Slot,
        data: &(Pubkey, AccountSharedData),
        id: SubscriptionID,
    ) -> Option<Bytes> {
        let (pubkey, account) = data;
        matches_filters(&self.filters, account.data()).then_some(())?;
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

pub(crate) struct TransactionResultEncoder;

impl TransactionResultEncoder {
    pub(crate) fn encode(
        &self,
        slot: Slot,
        data: &TransactionResult<()>,
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

pub(crate) struct SlotEncoder;

impl SlotEncoder {
    pub(crate) fn encode(
        &self,
        slot: Slot,
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
