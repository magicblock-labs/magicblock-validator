use std::collections::HashMap;

use geyser_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterTransactions, SubscribeUpdate,
    SubscribeUpdateAccount,
};
use sleipnir_rpc_client_api::config::{
    UiAccount, UiAccountEncoding, UiDataSliceConfig,
};
use solana_sdk::{account::Account, pubkey::Pubkey};

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

#[allow(unused)]
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
// Subscribe Account into UIAccount
// -----------------
pub fn subscribe_update_try_into_ui_account(
    update: SubscribeUpdate,
    encoding: UiAccountEncoding,
    data_slice_config: Option<UiDataSliceConfig>,
) -> Result<Option<UiAccount>, std::array::TryFromSliceError> {
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
) -> Result<Option<UiAccount>, std::array::TryFromSliceError> {
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
    Ok(Some(ui_account))
}
