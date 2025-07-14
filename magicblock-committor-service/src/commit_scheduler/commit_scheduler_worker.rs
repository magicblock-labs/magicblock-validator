use std::sync::{Arc, Mutex};

use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use tokio::sync::mpsc::{error::TryRecvError, Receiver};

use crate::{
    commit_scheduler::{
        commit_scheduler_inner::{CommitSchedulerInner, POISONED_INNER_MSG},
        db::DB,
        Error,
    },
    l1_message_executor::L1MessageExecutor,
    ComputeBudgetConfig,
};

pub(crate) struct CommitSchedulerWorker<D: DB> {
    db: Arc<D>,
    rpc_client: MagicblockRpcClient,
    table_mania: TableMania,
    compute_budget_config: ComputeBudgetConfig,
    receiver: Receiver<ScheduledL1Message>,

    inner: Arc<Mutex<CommitSchedulerInner>>,
}

impl<D: DB> CommitSchedulerWorker<D> {
    pub fn new(
        db: Arc<D>,
        rpc_client: MagicblockRpcClient,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
        receiver: Receiver<ScheduledL1Message>,
    ) -> Self {
        Self {
            db,
            rpc_client,
            table_mania,
            compute_budget_config,
            receiver,
            inner: Arc::new(Mutex::new(CommitSchedulerInner::new())),
        }
    }

    pub async fn start(mut self) {
        loop {
            let l1_message = match self.receiver.try_recv() {
                Ok(val) => val,
                Err(TryRecvError::Empty) => {
                    match self.get_or_wait_next_message().await {
                        Ok(val) => val,
                        Err(err) => panic!(err), // TODO(edwin): handle
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    // TODO(edwin): handle
                    panic!("Asdasd")
                }
            };

            self.handle_message(l1_message).await;
        }
    }

    async fn handle_message(&mut self, l1_message: ScheduledL1Message) {
        let l1_message = if let Some(l1_message) = self
            .inner
            .lock()
            .expect(POISONED_INNER_MSG)
            .schedule(l1_message)
        {
            l1_message
        } else {
            return;
        };

        let executor = L1MessageExecutor::new_v1(
            self.inner.clone(),
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.compute_budget_config.clone(),
        );
        tokio::spawn(executor.execute(l1_message, todo!()));
    }

    /// Return [`ScheduledL1Message`] from DB, otherwise waits on channel
    async fn get_or_wait_next_message(
        &mut self,
    ) -> Result<ScheduledL1Message, Error> {
        // Worker either cleaned-up congested channel and now need to clean-up DB
        // or we're just waiting on empty channel
        if let Some(l1_message) = self.db.pop_l1_message().await? {
            Ok(l1_message)
        } else {
            if let Some(val) = self.receiver.recv().await {
                Ok(val)
            } else {
                Err(Error::ChannelClosed)
            }
        }
    }
}

// Worker schedule:
// We have a pool of workers
// We are ready to accept message
// When we have a worker available to process it
