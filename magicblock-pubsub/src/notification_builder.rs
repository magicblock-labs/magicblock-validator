use magicblock_geyser_plugin::{grpc_messages::Message, types::GeyserMessage};
use serde::Serialize;
use solana_account_decoder::{encode_ui_account, UiAccount, UiAccountEncoding};
use solana_rpc_client_api::{
    filter::RpcFilterType,
    response::{ProcessedSignatureResult, RpcLogsResponse, RpcSignatureResult},
};
use solana_sdk::pubkey::Pubkey;

use crate::handler::common::UiAccountWithPubkey;

trait NotificationBuilder {
    type Notification: Serialize;
    fn try_build_notifcation(
        &self,
        msg: GeyserMessage,
    ) -> Option<Self::Notification>;
}

pub struct AccountNotificationBuilder {
    pub encoding: UiAccountEncoding,
}

impl NotificationBuilder for AccountNotificationBuilder {
    type Notification = UiAccount;

    fn try_build_notifcation(
        &self,
        msg: GeyserMessage,
    ) -> Option<Self::Notification> {
        let Message::Account(ref acc) = *msg else {
            return None;
        };
        Some(encode_ui_account(
            &self.pubkey,
            &acc.account,
            self.encoding,
            None,
            None,
        ))
    }
}

pub enum ProgramFilter {
    DataSize(usize),
    MemCmp { offset: usize, bytes: Vec<u8> },
}

pub struct ProgramFilters(Vec<ProgramFilters>);

impl ProgramFilter {
    fn matches(&self, data: &[u8]) -> bool {
        match self {
            Self::DataSize(len) => data.len() == len,
            Self::MemCmp { offset, bytes } => {
                if let Some(slice) = data.get(*offset..*offset + bytes.len()) {
                    slice == bytes
                } else {
                    false
                }
            }
        }
    }
}

impl ProgramFilters {
    #[inline]
    fn matches(&self, data: &[u8]) -> bool {
        self.0.iter().all(|f| f.matches(data))
    }
}

impl From<Option<Vec<RpcFilterType>>> for ProgramFilters {
    fn from(value: Option<Vec<RpcFilterType>>) -> Self {
        let Some(filters) = value else {
            return Self(vec![]);
        };
        let mut inner = Vec::with_capacity(filters.len());
        for f in filters {
            match f {
                RpcFilterType::DataSize(len) => {
                    inner.push(ProgramFilter::DataSize(len as usize));
                }
                RpcFilterType::Memcmp(memcmp) => {
                    inner.push(ProgramFilter::MemCmp {
                        offset: memcmp.offset(),
                        bytes: memcmp.bytes().to_owned(),
                    });
                }
                _ => continue,
            }
        }
        Self(inner)
    }
}

pub struct ProgramNotificationBuilder {
    pub encoding: UiAccountEncoding,
    pub filters: ProgramFilters,
}

impl NotificationBuilder for ProgramNotificationBuilder {
    type Notification = UiAccountWithPubkey;

    fn try_build_notifcation(
        &self,
        msg: GeyserMessage,
    ) -> Option<Self::Notification> {
        let Message::Account(ref acc) = *msg else {
            return None;
        };
        self.filters.matches(&acc.account.data).then(())?;
        let account = encode_ui_account(
            &self.pubkey,
            &acc.account,
            self.encoding,
            None,
            None,
        );
        Some(UiAccountWithPubkey {
            pubkey: acc.account.pubkey.to_string(),
            account,
        })
    }
}

pub struct SignatureNotificationBulider;

impl NotificationBuilder for SignatureNotificationBulider {
    type Notification = RpcSignatureResult;

    fn try_build_notifcation(
        &self,
        msg: GeyserMessage,
    ) -> Option<Self::Notification> {
        let Message::Transaction(ref txn) = *msg else {
            return None;
        };
        let err = txn.transaction.meta.status.err();
        let result = ProcessedSignatureResult { err };
        Some(RpcSignatureResult::ProcessedSignature(result))
    }
}

pub struct LogsNotificationBulider;

impl NotificationBuilder for SignatureNotificationBulider {
    type Notification = RpcLogsResponse;

    fn try_build_notifcation(
        &self,
        msg: GeyserMessage,
    ) -> Option<Self::Notification> {
        let Message::Transaction(ref txn) = *msg else {
            return None;
        };
        let err = txn.transaction.meta.status.err();
        let signature = txn.transaction.signature.to_string();
        let logs = txn.transaction.meta.log_messages.unwrap_or_default();

        Some(RpcLogsResponse {
            signature,
            err,
            logs,
        })
    }
}
