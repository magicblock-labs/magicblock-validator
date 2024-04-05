use std::collections::HashMap;

use geyser_grpc_proto::geyser::SubscribeRequestFilterTransactions;

pub fn geyser_sub_for_transaction_signature(
    signature: String,
) -> HashMap<String, SubscribeRequestFilterTransactions> {
    let tx_sub = SubscribeRequestFilterTransactions {
        vote: None,
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
