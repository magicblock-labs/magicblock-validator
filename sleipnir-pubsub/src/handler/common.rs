use geyser_grpc_proto::{geyser, tonic::Status};
use jsonrpc_pubsub::Sink;
use log::*;
use serde::{Deserialize, Serialize};
use sleipnir_rpc_client_api::config::{UiAccount, UiAccountEncoding};

use crate::{
    conversions::{slot_from_update, subscribe_update_try_into_ui_account},
    errors::sink_notify_error,
    types::{AccountDataConfig, ResponseWithSubscriptionId},
};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct UiAccountWithPubkey {
    pubkey: String,
    account: UiAccount,
}

/// Handles geyser update for account and program subscriptions.
/// Returns true if subscription has ended.
pub fn handle_account_geyser_update(
    sink: &Sink,
    subid: u64,
    update: Result<geyser::SubscribeUpdate, Status>,
    params: AccountDataConfig,
    include_pubkey: bool,
) -> bool {
    match update {
        Ok(update) => {
            debug!("Received geyser update: {:?}", update);

            let slot = slot_from_update(&update).unwrap_or(0);

            let encoding = params.encoding.unwrap_or(UiAccountEncoding::Base58);
            let (pubkey, ui_account) =
                match subscribe_update_try_into_ui_account(
                    update,
                    encoding,
                    params.data_slice_config,
                ) {
                    Ok(Some(ui_account)) => ui_account,
                    Ok(None) => {
                        debug!("No account data in update, skipping.");
                        return false;
                    }
                    Err(err) => {
                        let msg = format!(
                            "Failed to convert update to UiAccount: {:?}",
                            err
                        );
                        let failed_to_notify = sink_notify_error(sink, msg);
                        return failed_to_notify;
                    }
                };
            let notify_res = if include_pubkey {
                let res = ResponseWithSubscriptionId::new(
                    UiAccountWithPubkey {
                        pubkey: pubkey.to_string(),
                        account: ui_account,
                    },
                    slot,
                    subid,
                );
                debug!("Sending response: {:?}", res);
                sink.notify(res.into_params_map())
            } else {
                let res =
                    ResponseWithSubscriptionId::new(ui_account, slot, subid);
                debug!("Sending response: {:?}", res);
                sink.notify(res.into_params_map())
            };

            if let Err(err) = notify_res {
                debug!("Subscription has ended, finishing {:?}.", err);
                true
            } else {
                false
            }
        }
        Err(status) => sink_notify_error(
            sink,
            format!("Failed to receive signature update: {:?}", status),
        ),
    }
}
