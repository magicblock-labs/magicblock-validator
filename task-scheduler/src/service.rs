use std::sync::Arc;

use engine::Engine;
use hydra_api::{ephemeral::ID as EPHEMERAL_PROGRAM_ID, instruction::ephemeral};
use magic_root_interface::MagicRootInstruction;
use nucleus::shutdown::{ShutdownHandle, ShutdownReason};
use program::args::{CancelTaskRequest, ScheduleTaskRequest, TaskRequest};
use solana_account::ReadableAccount;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tokio::{
    select,
    sync::{Semaphore, mpsc},
};
use tracing::*;

use crate::{
    crank::{build_create_ix, crank_pubkey, interval_slots, is_valid_task_interval},
    errors::TaskSchedulerResult,
};

/// Serves runtime schedule/cancel requests by sending hydra transactions.
/// The service is stateless: it derives each task's crank PDA deterministically
/// from `(authority, task_id)`, so cancel and reschedule need no persisted
/// lookup.
pub struct TaskSchedulerService {
    /// Receives service messages the engine publishes once a transaction
    /// commits; task requests arrive on this stream.
    service_messages: mpsc::UnboundedReceiver<Vec<u8>>,
    /// Schedule/cancel worker; one request runs at a time.
    processor: Arc<Processor>,
}

/// Schedule/cancel worker, shared with the active request.
struct Processor {
    /// Reads account state and the latest blockhash/slot, and provides the
    /// validator identity that sponsors and signs crank transactions.
    engine: Engine,
    /// RPC client used to send transactions.
    /// Otherwise, accounts are not ensured.
    rpc_client: RpcClient,
    /// Slot interval of the validator, used to convert millisecond intervals
    /// into the slot-based cadence hydra expects.
    slot_interval: tokio::time::Duration,
}

impl TaskSchedulerService {
    /// Creates a new `TaskSchedulerService`.
    pub fn new(
        engine: Engine,
        self_rpc_url: String,
        slot_interval: tokio::time::Duration,
    ) -> TaskSchedulerResult<Self> {
        Ok(Self {
            service_messages: engine.transactions().subscribe_service_messages()?,
            processor: Arc::new(Processor {
                engine,
                rpc_client: RpcClient::new(self_rpc_url),
                slot_interval,
            }),
        })
    }

    /// Runs the task scheduler and reports its terminal status.
    pub async fn run(mut self, mut shutdown: ShutdownHandle) {
        self.run_loop(&shutdown).await;
        let reason = if shutdown.requested() {
            ShutdownReason::Signalled
        } else {
            ShutdownReason::Unexpected
        };
        shutdown.terminate(reason);
    }

    /// Main loop: serves runtime schedule/cancel requests until cancelled.
    async fn run_loop(&mut self, shutdown: &ShutdownHandle) {
        let submissions = Arc::new(Semaphore::new(1));
        loop {
            // Reserve in receive order, before inspecting crank state or spawning work.
            let Ok(permit) = (select! {
                permit = Arc::clone(&submissions).acquire_owned() => permit,
                _ = shutdown.signalled() => break,
            }) else {
                break;
            };
            select! {
                message = self.service_messages.recv() => {
                    let encoded = match message {
                        Some(encoded) => encoded,
                        None => {
                            info!("Service message stream closed, stopping task scheduler");
                            break;
                        }
                    };
                    // The stream carries every service message, not only task
                    // requests; anything that is not a `TaskRequest` is ignored.
                    let Ok(request) =
                        wincode::deserialize::<TaskRequest>(&encoded)
                    else {
                        continue;
                    };
                    let processor = Arc::clone(&self.processor);
                    tokio::spawn(async move {
                        let _permit = permit;
                        processor.process(request).await;
                    });
                }
                _ = shutdown.signalled() => {
                    break;
                }
            }
        }

        info!("TaskSchedulerService shutdown!");
    }
}

impl Processor {
    /// Processes a [TaskRequest] from the transaction executor.
    async fn process(&self, request: TaskRequest) {
        let task_id = request.id();
        let result = match request {
            TaskRequest::Schedule(task) => self.schedule(task).await,
            TaskRequest::Cancel(task) => self.cancel(task).await,
        };
        if let Err(e) = result {
            error!("Failed to process task request {}: {}", task_id, e);
        }
    }

    /// Creates and funds a crank, atomically replacing it on reschedule.
    async fn schedule(&self, task: ScheduleTaskRequest) -> TaskSchedulerResult<()> {
        if !is_valid_task_interval(task.execution_interval_millis) || task.iterations <= 0 {
            return Ok(());
        }
        let crank = crank_pubkey(&task.authority, task.id);
        let crank_exists = self.crank_exists(&crank);

        let start_slot = self.engine.blocks().current_slot();

        let interval_slots = interval_slots(task.execution_interval_millis, self.slot_interval);
        // `i64::MAX` iterations is how the magic API spells "run forever". Hydra
        // has its own sentinel for that — wire-level `0`, which `Create` stores
        // as `REMAINING_INFINITE` — and passing the raw count instead produces a
        // *finite* crank of ~9.2e18 executions.
        let iterations = if task.iterations == i64::MAX { 0 } else { task.iterations as u64 };

        let sponsor = self.engine.signer().pubkey();
        let create_ix = build_create_ix(
            &sponsor,
            &task.authority,
            task.id,
            crank,
            start_slot,
            interval_slots,
            iterations,
            &task.instructions,
        );

        let ixs = if crank_exists {
            let cancel_ix = ephemeral::cancel(sponsor, crank, sponsor);
            vec![cancel_ix, create_ix]
        } else {
            vec![create_ix]
        };

        self.submit(ixs).await?;
        debug!("Created hydra crank for task {}", task.id);
        Ok(())
    }

    /// Cancels the crank and returns its remaining lamports to the validator.
    async fn cancel(&self, request: CancelTaskRequest) -> TaskSchedulerResult<()> {
        let crank = crank_pubkey(&request.authority, request.task_id);
        let sponsor = self.engine.signer().pubkey();
        let cancel_ix = ephemeral::cancel(sponsor, crank, sponsor);
        self.submit(vec![cancel_ix]).await?;
        debug!("Cancelled hydra crank for task {}", request.task_id);
        Ok(())
    }

    /// Returns whether a hydra-owned crank account currently exists at `crank`.
    fn crank_exists(&self, crank: &Pubkey) -> bool {
        self.engine
            .accounts()
            .loader()
            .read(crank, |account| *account.owner() == EPHEMERAL_PROGRAM_ID)
            .ok()
            .flatten()
            .unwrap_or(false)
    }

    /// Signs `instructions` with the validator identity — the crank sponsor —
    /// and submits them. Self-RPC with preflight enabled waits for execution,
    /// so the next request observes this request's committed crank state.
    async fn submit(&self, mut instructions: Vec<Instruction>) -> TaskSchedulerResult<()> {
        let validator = self.engine.signer();
        instructions.push(MagicRootInstruction::Nonce(rand::random()).compose(validator.pubkey())?);
        let transaction = Transaction::new_signed_with_payer(
            &instructions,
            Some(&validator.pubkey()),
            &[validator],
            self.engine.blockhash(),
        );
        self.rpc_client.send_transaction(&transaction).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use engine::testkit::TestEngine;
    use nucleus::shutdown::{Service, ShutdownManager};

    use super::*;

    /// Builds a service wired to a real test engine and returns it alongside
    /// the engine, which the caller must keep alive.
    async fn test_service() -> (TestEngine, TaskSchedulerService) {
        let engine = TestEngine::new().await;
        let service = TaskSchedulerService::new(
            engine.clone(),
            "http://localhost:8899".into(),
            tokio::time::Duration::from_secs(1),
        )
        .unwrap();
        (engine, service)
    }

    #[tokio::test]
    async fn test_service_shuts_down_on_cancel() {
        mbv_core::logger::init_for_tests();

        let (_engine, service) = test_service().await;
        let mut shutdown = ShutdownManager::default();
        let handle = tokio::spawn(service.run(shutdown.handle(Service::TaskScheduler)));

        let _ = shutdown.terminate().await;
        tokio::time::timeout(tokio::time::Duration::from_secs(2), handle)
            .await
            .expect("service should shut down promptly")
            .expect("task should not panic");
    }
}
