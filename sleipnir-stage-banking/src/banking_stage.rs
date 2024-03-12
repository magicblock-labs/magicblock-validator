// NOTE: from core/src/banking_stage.rs

//! The `banking_stage` processes Transaction messages. It is intended to be used
//! to construct a software pipeline. The stage uses all available CPU cores and
//! can do its processing in parallel with signature verification on the GPU.
use std::{
    cmp, env,
    sync::{Arc, RwLock},
    thread,
    thread::{Builder, JoinHandle},
};

use crossbeam_channel::{unbounded, Receiver, Sender};
use log::warn;
use sleipnir_bank::bank::Bank;
use sleipnir_messaging::{
    packet_deserializer::PacketDeserializer, BankingPacketReceiver,
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_perf::data_budget::DataBudget;

use crate::{
    committer::Committer,
    consumer::{ConsumeWorker, Consumer},
    qos_service::QosService,
    scheduler::{
        prio_graph_scheduler::PrioGraphScheduler,
        scheduler_controller::SchedulerController,
        scheduler_error::SchedulerError,
    },
};

// Fixed thread size seems to be fastest on GCP setup
pub const NUM_THREADS: u32 = 6;
const MIN_THREADS_BANKING: u32 = 1;
const MIN_TOTAL_THREADS: u32 = MIN_THREADS_BANKING;

/// Stores the stage's thread handle and output receiver.
pub struct BankingStage {
    bank_thread_hdls: Vec<JoinHandle<()>>,
}

impl BankingStage {
    /// Create the stage using `bank`. Exit when `verified_receiver` is dropped.
    pub fn new(
        non_vote_receiver: BankingPacketReceiver,
        transaction_status_sender: Option<TransactionStatusSender>,
        log_messages_bytes_limit: Option<usize>,
        bank: Arc<Bank>,
        chunk_size: Option<usize>,
    ) -> Self {
        Self::new_num_threads(
            non_vote_receiver,
            Self::num_threads(),
            transaction_status_sender,
            log_messages_bytes_limit,
            bank,
            chunk_size,
        )
    }

    pub fn new_num_threads(
        non_vote_receiver: BankingPacketReceiver,
        num_threads: u32,
        transaction_status_sender: Option<TransactionStatusSender>,
        log_messages_bytes_limit: Option<usize>,
        bank: Arc<Bank>,
        chunk_size: Option<usize>,
    ) -> Self {
        Self::new_central_scheduler(
            non_vote_receiver,
            num_threads,
            transaction_status_sender,
            log_messages_bytes_limit,
            bank,
            chunk_size,
        )
    }

    pub fn new_central_scheduler(
        non_vote_receiver: BankingPacketReceiver,
        num_threads: u32,
        transaction_status_sender: Option<TransactionStatusSender>,
        log_messages_bytes_limit: Option<usize>,
        bank: Arc<Bank>,
        chunk_size: Option<usize>,
    ) -> Self {
        assert!(num_threads >= MIN_TOTAL_THREADS);
        // NOTE: omitted latest_unprocessed_votes
        // NOTE: omitted decision_maker

        let committer = Committer::new(transaction_status_sender.clone());
        // NOTE: omitted transaction_recorder

        // + 1 for the central scheduler thread
        let mut bank_thread_hdls = Vec::with_capacity(num_threads as usize + 1);

        // NOTE: omitted spawn legacy voting threads first: 1 gossip, 1 tpu

        // Create channels for communication between scheduler and workers
        let num_workers = num_threads;
        let (work_senders, work_receivers): (Vec<Sender<_>>, Vec<Receiver<_>>) =
            (0..num_workers).map(|_| unbounded()).unzip();
        let (finished_work_sender, finished_work_receiver) = unbounded();

        // Spawn the worker threads
        let mut worker_metrics = Vec::with_capacity(num_workers as usize);
        for (index, work_receiver) in work_receivers.into_iter().enumerate() {
            let id = index as u32;
            let consume_worker = ConsumeWorker::new(
                id,
                work_receiver,
                Consumer::new(
                    committer.clone(),
                    QosService::new(id),
                    log_messages_bytes_limit,
                ),
                finished_work_sender.clone(),
                bank.clone(),
            );

            worker_metrics.push(consume_worker.metrics_handle());
            bank_thread_hdls.push(
                Builder::new()
                    .name(format!("solCoWorker{id:02}"))
                    .spawn(move || {
                        let _ = consume_worker.run();
                    })
                    .unwrap(),
            )
        }

        // Spawn the central scheduler thread
        bank_thread_hdls.push({
            let packet_deserializer =
                PacketDeserializer::new(non_vote_receiver);
            let scheduler =
                PrioGraphScheduler::new(work_senders, finished_work_receiver);
            let scheduler_controller = SchedulerController::new(
                packet_deserializer,
                bank.clone(),
                scheduler,
                worker_metrics,
                chunk_size,
            );
            Builder::new()
                .name("solBnkTxSched".to_string())
                .spawn(move || match scheduler_controller.run() {
                    Ok(_) => {}
                    Err(SchedulerError::DisconnectedRecvChannel(_)) => {}
                    Err(SchedulerError::DisconnectedSendChannel(_)) => {
                        warn!("Unexpected worker disconnect from scheduler")
                    }
                })
                .unwrap()
        });

        Self { bank_thread_hdls }
    }

    pub fn num_threads() -> u32 {
        cmp::max(
            env::var("SOLANA_BANKING_THREADS")
                .map(|x| x.parse().unwrap_or(NUM_THREADS))
                .unwrap_or(NUM_THREADS),
            MIN_TOTAL_THREADS,
        )
    }

    pub fn join(self) -> thread::Result<()> {
        for bank_thread_hdl in self.bank_thread_hdls {
            bank_thread_hdl.join()?;
        }
        Ok(())
    }
}
