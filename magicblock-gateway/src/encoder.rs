use hyper::body::Bytes;
use json::Serialize;
use magicblock_gateway_types::{
    accounts::{AccountSharedData, Pubkey, ReadableAccount},
    transactions::{
        TransactionProcessingResult, TransactionResult, TransactionStatus,
    },
};
use solana_account_decoder::{encode_ui_account, UiAccountEncoding};

use crate::{
    requests::{params::SerdeSignature, payload::NotificationPayload},
    state::subscriptions::SubscriptionID,
    utils::{AccountWithPubkey, ProgramFilters},
    Slot,
};

pub(crate) trait Encoder: Ord + Eq + Clone {
    type Data;
    fn encode(&self, slot: u64, data: &Self::Data, id: u64) -> Option<Bytes>;
}

#[derive(Debug, PartialEq, PartialOrd, Eq, Ord, Clone)]
pub(crate) enum AccountEncoder {
    Base58,
    Base64,
    Base64Zstd,
    JsonParsed,
}

impl From<&AccountEncoder> for UiAccountEncoding {
    fn from(value: &AccountEncoder) -> Self {
        match value {
            AccountEncoder::Base58 => Self::Base58,
            AccountEncoder::Base64 => Self::Base64,
            AccountEncoder::Base64Zstd => Self::Base64Zstd,
            AccountEncoder::JsonParsed => Self::JsonParsed,
        }
    }
}

impl From<&UiAccountEncoding> for AccountEncoder {
    fn from(value: &UiAccountEncoding) -> Self {
        match value {
            UiAccountEncoding::Base58 | UiAccountEncoding::Binary => {
                Self::Base58
            }
            UiAccountEncoding::Base64 => Self::Base64,
            UiAccountEncoding::Base64Zstd => Self::Base64Zstd,
            UiAccountEncoding::JsonParsed => Self::JsonParsed,
        }
    }
}

#[derive(PartialEq, PartialOrd, Ord, Eq, Clone)]
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
        let encoded =
            encode_ui_account(&data.0, &data.1, self.into(), None, None);
        let method = "accountNotification";
        NotificationPayload::encode(encoded, slot, method, id)
    }
}

impl Encoder for ProgramAccountEncoder {
    type Data = (Pubkey, AccountSharedData);

    fn encode(&self, slot: Slot, data: &Self::Data, id: u64) -> Option<Bytes> {
        self.filters.matches(data.1.data()).then_some(())?;
        let value = AccountWithPubkey::new(
            data.0,
            &data.1,
            (&self.encoder).into(),
            None,
        );
        let method = "programNotification";
        NotificationPayload::encode(value, slot, method, id)
    }
}

#[derive(PartialEq, PartialOrd, Ord, Eq, Clone)]
pub(crate) struct TransactionResultEncoder;

impl Encoder for TransactionResultEncoder {
    type Data = TransactionResult;

    fn encode(
        &self,
        slot: Slot,
        data: &Self::Data,
        id: SubscriptionID,
    ) -> Option<Bytes> {
        #[derive(Serialize)]
        struct SignatureResult {
            err: Option<String>,
        }
        let method = "signatureNotification";
        let err = data.as_ref().map_err(|e| e.to_string()).err();
        let result = SignatureResult { err };
        NotificationPayload::encode(result, slot, method, id)
    }
}

#[derive(PartialEq, PartialOrd, Ord, Eq, Clone)]
pub(crate) enum TransactionLogsEncoder {
    All,
    Mentions(Pubkey),
}

impl Encoder for TransactionLogsEncoder {
    type Data = TransactionStatus;

    fn encode(
        &self,
        slot: Slot,
        data: &Self::Data,
        id: SubscriptionID,
    ) -> Option<Bytes> {
        let TransactionProcessingResult::Execution(execution) = &data.result
        else {
            return None;
        };
        if let Self::Mentions(pubkey) = self {
            execution
                .accounts
                .iter()
                .any(|p| p == pubkey)
                .then_some(())?;
        }
        #[derive(Serialize)]
        struct TransactionLogs<'a> {
            signature: SerdeSignature,
            err: Option<String>,
            logs: &'a [String],
        }
        let method = "logsNotification";
        let result = TransactionLogs {
            signature: SerdeSignature(data.signature),
            err: execution.result.as_ref().map_err(|e| e.to_string()).err(),
            logs: &execution.logs,
        };
        NotificationPayload::encode(result, slot, method, id)
    }
}

#[derive(PartialEq, PartialOrd, Ord, Eq, Clone)]
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
