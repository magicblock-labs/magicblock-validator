use std::sync::{atomic::Ordering, Arc};

use crossbeam_channel::{Receiver, RecvError, SendError, Sender};
use sleipnir_bank::bank::Bank;
use thiserror::Error;

use crate::{
    messages::scheduler_messages::{ConsumeWork, FinishedConsumeWork},
    metrics::ConsumeWorkerMetrics,
};

use super::Consumer;

#[derive(Debug, Error)]
pub enum ConsumeWorkerError {
    #[error("Failed to receive work from scheduler: {0}")]
    Recv(#[from] RecvError),
    #[error("Failed to send finalized consume work to scheduler: {0}")]
    Send(#[from] SendError<FinishedConsumeWork>),
}

// NOTE:
// replaced leader_bank_notifier: Arc<LeaderBankNotifier>,
// with: bank: Arc<Bank>,
pub(crate) struct ConsumeWorker {
    consume_receiver: Receiver<ConsumeWork>,
    consumer: Consumer,
    consumed_sender: Sender<FinishedConsumeWork>,

    metrics: Arc<ConsumeWorkerMetrics>,

    bank: Arc<Bank>,
}

impl ConsumeWorker {
    pub fn new(
        id: u32,
        consume_receiver: Receiver<ConsumeWork>,
        consumer: Consumer,
        consumed_sender: Sender<FinishedConsumeWork>,
        bank: Arc<Bank>,
    ) -> Self {
        Self {
            consume_receiver,
            consumer,
            consumed_sender,
            bank,
            metrics: Arc::new(ConsumeWorkerMetrics::new(id)),
        }
    }

    pub fn metrics_handle(&self) -> Arc<ConsumeWorkerMetrics> {
        self.metrics.clone()
    }

    pub fn run(self) -> Result<(), ConsumeWorkerError> {
        loop {
            let work = self.consume_receiver.recv()?;
            self.consume_loop(work)?;
        }
    }

    fn consume_loop(&self, work: ConsumeWork) -> Result<(), ConsumeWorkerError> {
        // NOTE: removed get_consumer_bank from leader_bank_notifier with timeout

        for work in try_drain_iter(work, &self.consume_receiver) {
            // NOTE: removed bank.is_complete() check
            self.consume(&self.bank, work)?;
        }

        Ok(())
    }

    /// Consume a single batch.
    fn consume(&self, bank: &Arc<Bank>, work: ConsumeWork) -> Result<(), ConsumeWorkerError> {
        let output = self.consumer.process_and_record_aged_transactions(
            bank,
            &work.transactions,
            &work.max_age_slots,
        );

        self.metrics.update_for_consume(&output);
        self.metrics.has_data.store(true, Ordering::Relaxed);

        self.consumed_sender.send(FinishedConsumeWork {
            work,
            retryable_indexes: output
                .execute_and_commit_transactions_output
                .retryable_transaction_indexes,
        })?;
        Ok(())
    }

    // NOTE: removed retry and retry_drain
}

/// Helper function to create an non-blocking iterator over work in the receiver,
/// starting with the given work item.
fn try_drain_iter<T>(work: T, receiver: &Receiver<T>) -> impl Iterator<Item = T> + '_ {
    std::iter::once(work).chain(receiver.try_iter())
}
