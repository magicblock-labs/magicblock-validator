use engine::Engine;
use hydra_api::{
    consts::ephemeral::CRANKER_REWARD, ephemeral::ID as EPHEMERAL_PROGRAM_ID,
    instruction::ephemeral,
};
use magicblock_program::args::{
    CancelTaskRequest, ScheduleTaskRequest, TaskRequest,
};
use solana_account::ReadableAccount;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tokio::{select, sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::*;

use crate::{
    crank::{
        build_create_ix, crank_pubkey, crank_rent_floor, interval_slots,
        is_valid_task_interval,
    },
    errors::{TaskSchedulerError, TaskSchedulerResult},
};

/// Serves runtime schedule/cancel requests by sending hydra transactions.
/// The service is stateless: it derives each task's crank PDA deterministically
/// from `(authority, task_id)`, so cancel and reschedule need no persisted
/// lookup.
pub struct TaskSchedulerService {
    /// Delegated faucet keypair used to pay for (sponsor, fund, sign, and
    /// cancel) hydra cranks. Used instead of the validator identity, which is
    /// not a delegated account.
    faucet: Keypair,
    /// Receives service messages the engine publishes once a transaction
    /// commits; task requests arrive on this stream.
    service_messages: mpsc::Receiver<Vec<u8>>,
    /// Reads account state and the latest blockhash/slot, and submits the
    /// faucet-signed crank transactions.
    engine: Engine,
    /// Token used to cancel the task scheduler.
    token: CancellationToken,
    /// Slot interval of the validator, used to convert millisecond intervals
    /// into the slot-based cadence hydra expects.
    slot_interval: tokio::time::Duration,
}

// SAFETY: TaskSchedulerService is moved into a single Tokio task in `start()`
// and never cloned. It runs exclusively on that task. All fields are Send+Sync.
unsafe impl Send for TaskSchedulerService {}
unsafe impl Sync for TaskSchedulerService {}
impl TaskSchedulerService {
    /// Creates a new `TaskSchedulerService`.
    pub fn new(
        faucet: Keypair,
        engine: Engine,
        slot_interval: tokio::time::Duration,
        token: CancellationToken,
    ) -> TaskSchedulerResult<Self> {
        Ok(Self {
            faucet,
            service_messages: engine
                .transactions()
                .subscribe_service_messages()?,
            engine,
            token,
            slot_interval,
        })
    }

    /// Starts the `TaskSchedulerService` and returns a handle to the task.
    pub async fn start(
        self,
    ) -> TaskSchedulerResult<JoinHandle<TaskSchedulerResult<()>>> {
        Ok(tokio::spawn(self.run()))
    }

    /// Main loop: serves runtime schedule/cancel requests until cancelled.
    async fn run(mut self) -> TaskSchedulerResult<()> {
        loop {
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
                    self.process_request(request).await;
                }
                _ = self.token.cancelled() => {
                    break;
                }
            }
        }

        info!("TaskSchedulerService shutdown!");
        Ok(())
    }

    /// Processes a [TaskRequest] from the transaction executor.
    async fn process_request(&self, request: TaskRequest) {
        let task_id = request.id();
        let result = match request {
            TaskRequest::Schedule(schedule_request) => {
                self.process_schedule_request(schedule_request).await
            }
            TaskRequest::Cancel(cancel_request) => {
                self.process_cancel_request(&cancel_request).await
            }
        };
        if let Err(e) = result {
            error!("Failed to process task request {}: {}", task_id, e);
        }
    }

    /// Schedules a task: creates and funds its hydra crank.
    async fn process_schedule_request(
        &self,
        task: ScheduleTaskRequest,
    ) -> TaskSchedulerResult<()> {
        if !is_valid_task_interval(task.execution_interval_millis) {
            // Too large or zero: ignore.
            return Ok(());
        }
        let interval_millis =
            task.execution_interval_millis.clamp(1, u32::MAX as i64);

        self.schedule_crank(
            &task.authority,
            task.id,
            interval_millis,
            task.iterations,
            &task.instructions,
        )
        .await?;
        debug!("Created hydra crank for task {}", task.id);
        Ok(())
    }

    /// Cancels a task's hydra crank, if one exists for `(authority, task_id)`.
    async fn process_cancel_request(
        &self,
        cancel_request: &CancelTaskRequest,
    ) -> TaskSchedulerResult<()> {
        let crank =
            crank_pubkey(&cancel_request.authority, cancel_request.task_id);

        // Does not check if the crank exists, so it will fail if it does not exist
        self.send_cancel(crank).await?;
        debug!("Cancelled hydra crank for task {}", cancel_request.task_id);

        Ok(())
    }

    /// Creates and funds the hydra crank for a task. If a crank already exists
    /// at the deterministic PDA (a reschedule), it is closed first so the new
    /// schedule can recreate it.
    async fn schedule_crank(
        &self,
        authority: &Pubkey,
        task_id: i64,
        interval_millis: i64,
        iterations: i64,
        instructions: &[Instruction],
    ) -> TaskSchedulerResult<()> {
        if iterations <= 0 {
            return Ok(());
        }
        self.send_create(
            authority,
            task_id,
            interval_millis,
            iterations,
            instructions,
        )
        .await?;
        Ok(())
    }

    /// Returns whether a hydra-owned crank account currently exists at `crank`.
    fn crank_exists(&self, crank: &Pubkey) -> bool {
        matches!(
            self.engine.accounts().get(crank),
            Ok(Some(account)) if *account.owner() == EPHEMERAL_PROGRAM_ID
        )
    }

    /// Builds and sends the transaction that creates and funds a hydra crank.
    /// It cancels the crank first if it already exists.
    async fn send_create(
        &self,
        authority: &Pubkey,
        task_id: i64,
        interval_millis: i64,
        iterations: i64,
        instructions: &[Instruction],
    ) -> TaskSchedulerResult<()> {
        let crank = crank_pubkey(authority, task_id);
        let crank_exists = self.crank_exists(&crank);

        let start_slot = self.engine.blocks().current_slot();

        let interval_slots =
            interval_slots(interval_millis, self.slot_interval);
        let iterations = iterations as u64;

        let faucet_pubkey = self.faucet.pubkey();
        let create_ix = build_create_ix(
            &faucet_pubkey,
            authority,
            task_id,
            crank,
            start_slot,
            interval_slots,
            iterations,
            instructions,
        );
        let reward_pool = iterations.saturating_mul(CRANKER_REWARD);
        let funding =
            reward_pool.saturating_add(crank_rent_floor(instructions));
        let fund_ix = solana_system_interface::instruction::transfer(
            &faucet_pubkey,
            &crank,
            funding,
        );

        let ixs = if crank_exists {
            let cancel_ix =
                ephemeral::cancel(faucet_pubkey, crank, faucet_pubkey);
            vec![cancel_ix, create_ix, fund_ix]
        } else {
            vec![create_ix, fund_ix]
        };

        self.submit(&ixs).await
    }

    /// Sends a crank cancellation. The crank's remaining lamports are returned
    /// to the faucet (the cancel recipient).
    async fn send_cancel(&self, crank: Pubkey) -> TaskSchedulerResult<()> {
        let faucet_pubkey = self.faucet.pubkey();
        let cancel_ix = ephemeral::cancel(faucet_pubkey, crank, faucet_pubkey);
        self.submit(&[cancel_ix]).await
    }

    /// Signs `instructions` with the faucet — the crank sponsor, which the
    /// engine authority cannot stand in for — and submits them.
    /// Send and forget since the write lock on the faucet prevents races.
    async fn submit(
        &self,
        instructions: &[Instruction],
    ) -> TaskSchedulerResult<()> {
        let faucet_pubkey = self.faucet.pubkey();
        let transaction = Transaction::new_signed_with_payer(
            instructions,
            Some(&faucet_pubkey),
            &[&self.faucet],
            self.engine.blockhash(),
        );
        self.engine
            .transaction(transaction)
            .map_err(TaskSchedulerError::from)?
            .schedule()
            .await
            .map_err(TaskSchedulerError::from)
    }
}

#[cfg(test)]
mod tests {
    use engine::testkit::TestEngine;

    use super::*;

    /// Builds a service wired to a real test engine and returns it alongside
    /// the engine, which the caller must keep alive.
    async fn test_service() -> (TestEngine, TaskSchedulerService) {
        let engine = TestEngine::new().await;
        let service = TaskSchedulerService {
            faucet: Keypair::new(),
            service_messages: engine
                .transactions()
                .subscribe_service_messages()
                .unwrap(),
            engine: engine.clone(),
            token: CancellationToken::new(),
            slot_interval: tokio::time::Duration::from_millis(1000),
        };
        (engine, service)
    }

    #[tokio::test]
    async fn test_service_shuts_down_on_cancel() {
        magicblock_core::logger::init_for_tests();

        let (_engine, service) = test_service().await;
        let token = service.token.clone();
        let handle = service.start().await.unwrap();

        token.cancel();
        tokio::time::timeout(tokio::time::Duration::from_secs(2), handle)
            .await
            .expect("service should shut down promptly")
            .expect("task should not panic")
            .expect("run() should return Ok");
    }
}
