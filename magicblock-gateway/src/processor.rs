use std::sync::Arc;

use log::info;
use tokio_util::sync::CancellationToken;

use crate::state::{
    blocks::BlocksCache,
    subscriptions::SubscriptionsDb,
    transactions::{SignatureStatus, TransactionsCache},
    SharedState,
};

pub(crate) struct EventProcessor {
    subscriptions: SubscriptionsDb,
    transactions: TransactionsCache,
    blocks: Arc<BlocksCache>,
    account_update_rx: AccountUpdateRx,
    transaction_status_rx: TxnStatusRx,
    block_update_rx: BlockUpdateRx,
}

impl EventProcessor {
    fn new(channels: &RpcChannelEndpoints, state: &SharedState) -> Self {
        Self {
            subscriptions: state.subscriptions.clone(),
            transactions: state.transactions.clone(),
            blocks: state.blocks.clone(),
            account_update_rx: channels.account_update_rx.clone(),
            transaction_status_rx: channels.transaction_status_rx.clone(),
            block_update_rx: channels.block_update_rx.clone(),
        }
    }

    pub(crate) fn start(
        state: &SharedState,
        channels: &RpcChannelEndpoints,
        instances: usize,
        cancel: CancellationToken,
    ) {
        for id in 0..instances {
            let processor = EventProcessor::new(channels, state);
            tokio::spawn(processor.run(id, cancel.clone()));
        }
    }

    async fn run(self, id: usize, cancel: CancellationToken) {
        info!("event processor {id} is running");
        loop {
            tokio::select! {
                biased; Ok(status) = self.transaction_status_rx.recv_async() => {
                    let result = &status.result.result;
                    self.subscriptions.send_signature_update(&status.signature, result, status.slot).await;
                    self.subscriptions.send_logs_update(&status, status.slot);
                    self.transactions.push(
                        status.signature,
                        SignatureStatus { slot: status.slot, successful: result.is_ok() }
                    );
                }
                Ok(state) = self.account_update_rx.recv_async() => {
                    self.subscriptions.send_account_update(&state).await;
                    self.subscriptions.send_program_update(&state).await;
                }
                Ok(latest) = self.block_update_rx.recv_async() => {
                    self.subscriptions.send_slot(latest.meta.slot);
                    self.blocks.set_latest(latest);
                }
                _ = cancel.cancelled() => {
                    break;
                }
            }
        }
        info!("event processor {id} has terminated");
    }
}
