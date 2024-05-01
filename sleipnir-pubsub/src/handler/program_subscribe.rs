use jsonrpc_pubsub::Subscriber;
use log::*;
use sleipnir_geyser_plugin::rpc::GeyserRpcService;
use tokio_util::sync::CancellationToken;

use crate::{
    conversions::try_geyser_sub_for_program, errors::reject_internal_error,
    handler::common::handle_account_geyser_update, subscription::assign_sub_id,
    types::ProgramParams,
};

pub async fn handle_program_subscribe(
    subid: u64,
    subscriber: Subscriber,
    unsubscriber: CancellationToken,
    params: &ProgramParams,
    geyser_service: &GeyserRpcService,
) {
    let address = params.program_id();
    let config = params.config();

    let sub = match try_geyser_sub_for_program(address.to_string(), config) {
        Ok(sub) => sub,
        Err(err) => {
            reject_internal_error(subscriber, "Invalid config", Some(err));
            return;
        }
    };

    info!("Sub {:#?}", sub);

    let mut geyser_rx =
        match geyser_service.accounts_subscribe(sub, subid, unsubscriber, None)
        {
            Ok(res) => res,
            Err(err) => {
                reject_internal_error(
                    subscriber,
                    "Failed to subscribe to signature",
                    Some(err),
                );
                return;
            }
        };

    if let Some(sink) = assign_sub_id(subscriber, subid) {
        loop {
            tokio::select! {
                val = geyser_rx.recv() => {
                    match val {
                        Some(update) => {
                            if handle_account_geyser_update(
                                &sink,
                                subid,
                                update,
                                params.into(),
                                true,
                            ) {
                                break;
                            }
                        }
                        None => {
                            debug!(
                                "Geyser subscription has ended, finishing."
                            );
                            break;
                        }
                    }
                }
            }
        }
    }
}
