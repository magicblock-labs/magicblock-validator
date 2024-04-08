use std::collections::HashMap;

use geyser_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SubscribeRequestFilterTransactions,
    SubscribeUpdate,
};

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

pub fn slot_from_update(update: &SubscribeUpdate) -> Option<u64> {
    update.update_oneof.as_ref().map(|oneof| {
        use UpdateOneof::*;
        match oneof {
            Account(_) => todo!("slot_from_update.Account"),
            Slot(slot) => slot.slot,
            Transaction(tx) => tx.slot,
            Block(_) => todo!("slot_from_update.Block"),
            Ping(_) => todo!("slot_from_update.Ping"),
            Pong(_) => todo!("slot_from_update.Pong"),
            BlockMeta(_) => todo!("slot_from_update.BlockMeta"),
            Entry(_) => todo!("slot_from_update.Entry"),
        }
    })
}
