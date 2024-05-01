use std::collections::HashMap;

use geyser_grpc_proto::geyser::{
    subscribe_request_filter_accounts_filter::Filter,
    subscribe_request_filter_accounts_filter_memcmp::Data,
    subscribe_update::UpdateOneof, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterAccountsFilter,
    SubscribeRequestFilterAccountsFilterMemcmp, SubscribeRequestFilterSlots,
    SubscribeRequestFilterTransactions, SubscribeUpdate,
    SubscribeUpdateAccount,
};
use sleipnir_rpc_client_api::{
    config::{
        RpcProgramAccountsConfig, RpcTransactionLogsFilter, UiAccount,
        UiAccountEncoding, UiDataSliceConfig,
    },
    filter::{MemcmpEncodedBytes, RpcFilterType},
    response::RpcLogsResponse,
};
use solana_sdk::{account::Account, pubkey::Pubkey, signature::Signature};

use crate::{
    errors::{PubsubError, PubsubResult},
    types::SlotResponse,
};

// -----------------
// geyser_sub_for_transaction_signature
// -----------------
pub fn geyser_sub_for_transaction_signature(
    signature: String,
) -> HashMap<String, SubscribeRequestFilterTransactions> {
    let tx_sub = SubscribeRequestFilterTransactions {
        vote: Some(false),
        failed: None,
        signature: Some(signature),
        account_include: vec![],
        account_exclude: vec![],
        account_required: vec![],
    };
    let mut map = HashMap::new();
    map.insert("transaction_signature".to_string(), tx_sub);
    map
}

pub fn try_geyser_sub_for_transaction_logs(
    logs_filter: &RpcTransactionLogsFilter,
) -> PubsubResult<HashMap<String, SubscribeRequestFilterTransactions>> {
    let mut tx_sub = SubscribeRequestFilterTransactions {
        vote: None,
        failed: None,
        signature: None,
        account_include: vec![],
        account_exclude: vec![],
        account_required: vec![],
    };

    use RpcTransactionLogsFilter::*;
    match logs_filter {
        All => tx_sub.vote = Some(false),
        AllWithVotes => tx_sub.vote = Some(true),
        Mentions(accs) => {
            if accs.len() > 1 {
                return Err(PubsubError::InvalidParam(
                    "Only one account mention can be specified in logs filter."
                        .to_string(),
                    format!("{:?}", accs),
                ));
            }
            tx_sub.account_required = accs.clone();
        }
    }

    let mut map = HashMap::new();
    map.insert("transaction_logs".to_string(), tx_sub);
    Ok(map)
}

// -----------------
// geyser_sub_for_account
// -----------------
pub fn geyser_sub_for_account(
    account: String,
) -> HashMap<String, SubscribeRequestFilterAccounts> {
    let account_sub = SubscribeRequestFilterAccounts {
        account: vec![account],
        owner: vec![],
        filters: vec![],
    };
    let mut map = HashMap::new();
    map.insert("account".to_string(), account_sub);
    map
}

// -----------------
// try_geyser_sub_for_program
// -----------------
pub fn try_geyser_sub_for_program(
    program_id: String,
    config: &Option<RpcProgramAccountsConfig>,
) -> PubsubResult<HashMap<String, SubscribeRequestFilterAccounts>> {
    let filters = config
        .as_ref()
        .and_then(|config| config.filters.as_ref())
        .map(|filters| {
            filters
                .iter()
                .map(try_rpc_filter_into_geyser_account_filter)
                .collect::<PubsubResult<Vec<SubscribeRequestFilterAccountsFilter>>>()
        })
        .transpose()?
        .unwrap_or_default();

    let account_sub = SubscribeRequestFilterAccounts {
        account: vec![],
        owner: vec![program_id],
        filters,
    };
    let mut map = HashMap::new();
    map.insert("program".to_string(), account_sub);
    Ok(map)
}

fn try_rpc_filter_into_geyser_account_filter(
    rpc_filter: &RpcFilterType,
) -> PubsubResult<SubscribeRequestFilterAccountsFilter> {
    match rpc_filter {
        RpcFilterType::Memcmp(memcmp) => {
            #[allow(deprecated)]
            let offset = memcmp.offset.try_into().map_err(|e| {
                PubsubError::InvalidParam(
                    "Program Account memcmp offset needs to be a u64."
                        .to_string(),
                    format!("{e:?}"),
                )
            })?;

            let has_data = memcmp
                .bytes()
                .as_ref()
                .map(|bytes| bytes.is_empty())
                .unwrap_or_default();

            let data = if has_data {
                use MemcmpEncodedBytes::*;
                #[allow(deprecated)]
                Some(match &memcmp.bytes {
                    Bytes(bytes) => Data::Bytes(bytes.to_vec()),
                    Binary(bytes) | Base58(bytes) => {
                        Data::Base58(bytes.to_string())
                    }
                    Base64(base64) => Data::Base64(base64.to_string()),
                })
            } else {
                None
            };
            let filter =
                SubscribeRequestFilterAccountsFilterMemcmp { offset, data };
            Ok(SubscribeRequestFilterAccountsFilter {
                filter: Some(Filter::Memcmp(filter)),
            })
        }
        RpcFilterType::DataSize(size) => {
            Ok(SubscribeRequestFilterAccountsFilter {
                filter: Some(Filter::Datasize(*size)),
            })
        }
        RpcFilterType::TokenAccountState => {
            Ok(SubscribeRequestFilterAccountsFilter {
                filter: Some(Filter::TokenAccountState(true)),
            })
        }
    }
}

pub fn geyser_sub_for_slot_update(
) -> HashMap<String, SubscribeRequestFilterSlots> {
    let slot_sub = SubscribeRequestFilterSlots {
        filter_by_commitment: Some(false),
    };
    let mut map = HashMap::new();
    map.insert("slot".to_string(), slot_sub);
    map
}

pub fn slot_from_update(update: &SubscribeUpdate) -> Option<u64> {
    update.update_oneof.as_ref().and_then(|oneof| {
        use UpdateOneof::*;
        match oneof {
            Account(acc) => Some(acc.slot),
            Slot(slot) => Some(slot.slot),
            Transaction(tx) => Some(tx.slot),
            Block(block) => Some(block.slot),
            Ping(_) => None,
            Pong(_) => None,
            BlockMeta(block_meta) => Some(block_meta.slot),
            Entry(entry) => Some(entry.slot),
        }
    })
}

// -----------------
// Subscribe Update into SlotResponse
// -----------------
pub fn subscribe_update_into_slot_response(
    update: SubscribeUpdate,
) -> Option<SlotResponse> {
    update.update_oneof.and_then(|oneof| {
        use UpdateOneof::*;
        match oneof {
            Account(_) => None,
            Slot(slot) => Some(SlotResponse {
                parent: slot.parent(),
                // We have a single bank
                root: slot.slot,
                slot: slot.slot,
            }),
            Transaction(_) => None,
            Block(_) => None,
            Ping(_) => None,
            Pong(_) => None,
            BlockMeta(_) => None,
            Entry(_) => None,
        }
    })
}

// -----------------
// Subscribe Update into UIAccount
// -----------------
pub fn subscribe_update_try_into_ui_account(
    update: SubscribeUpdate,
    encoding: UiAccountEncoding,
    data_slice_config: Option<UiDataSliceConfig>,
) -> Result<Option<(Pubkey, UiAccount)>, std::array::TryFromSliceError> {
    match subscribe_update_into_update_account(update) {
        Some(acc) => ui_account_from_subscribe_account_info(
            acc,
            encoding,
            data_slice_config,
        ),
        None => Ok(None),
    }
}

fn subscribe_update_into_update_account(
    update: SubscribeUpdate,
) -> Option<SubscribeUpdateAccount> {
    update.update_oneof.and_then(|oneof| {
        use UpdateOneof::*;
        match oneof {
            Account(acc) => Some(acc),
            Slot(_) => None,
            Transaction(_) => None,
            Block(_) => None,
            Ping(_) => None,
            Pong(_) => None,
            BlockMeta(_) => None,
            Entry(_) => None,
        }
    })
}

fn ui_account_from_subscribe_account_info(
    sub_acc: SubscribeUpdateAccount,
    encoding: UiAccountEncoding,
    data_slice_config: Option<UiDataSliceConfig>,
) -> Result<Option<(Pubkey, UiAccount)>, std::array::TryFromSliceError> {
    let inner_acc = match sub_acc.account {
        Some(acc) => acc,
        None => return Ok(None),
    };

    let pubkey = Pubkey::try_from(inner_acc.pubkey.as_slice())?;
    let owner = Pubkey::try_from(inner_acc.owner.as_slice())?;
    let account = Account {
        lamports: inner_acc.lamports,
        data: inner_acc.data,
        owner,
        executable: inner_acc.executable,
        rent_epoch: inner_acc.rent_epoch,
    };
    let ui_account =
        UiAccount::encode(&pubkey, &account, encoding, None, data_slice_config);
    Ok(Some((pubkey, ui_account)))
}

// -----------------
// Subscribe Update into Logs
// -----------------
pub fn try_subscribe_update_into_logs(
    update: SubscribeUpdate,
) -> PubsubResult<Option<RpcLogsResponse>> {
    use UpdateOneof::*;
    let tx = match update.update_oneof {
        Some(Transaction(tx)) => match tx.transaction {
            Some(tx) => tx,
            None => return Ok(None),
        },
        _ => return Ok(None),
    };

    let signature = Signature::try_from(tx.signature)
        .map_err(|err| {
            PubsubError::InvalidTransactionSignature(format!("{:?}", err))
        })?
        .to_string();

    let meta = match tx.meta {
        Some(meta) => meta,
        None => return Ok(None),
    };

    let logs = meta.log_messages;
    let err = meta
        .err
        .map(|err| bincode::deserialize(&err.err))
        .transpose()
        .map_err(|err| {
            PubsubError::CouldNotConvertTransactionError(err.to_string())
        })?;

    Ok(Some(RpcLogsResponse {
        signature,
        err,
        logs,
    }))
}
