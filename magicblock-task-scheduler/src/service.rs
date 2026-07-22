use std::sync::Arc;

use hydra_api::{
    consts::CRANKER_REWARD, ephemeral::ID as EPHEMERAL_PROGRAM_ID,
    instruction::ephemeral,
};
use magicblock_core::link::transactions::ScheduledTasksRx;
use magicblock_ledger::LatestBlock;
use magicblock_program::{
    args::{CancelTaskRequest, ScheduleTaskRequest, TaskRequest},
    validator::{validator_authority, validator_authority_id},
};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tokio::{select, task::JoinHandle};
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
/// Tasks persisted by the legacy (validator-funded) scheduler are migrated
/// onto hydra separately, out of band (see [`crate::migration`]) — this
/// service is stateless at runtime and derives each task's crank PDA
/// deterministically from `(authority, task_id)`.
pub struct TaskSchedulerService {
    /// RPC client used to send hydra transactions.
    rpc_client: Arc<RpcClient>,
    /// Delegated faucet keypair used to pay for (sponsor, fund, sign, and
    /// cancel) hydra cranks. Used instead of the validator identity, which is
    /// not a delegated account.
    faucet: Keypair,
    /// Used to receive scheduled tasks from the transaction executor.
    scheduled_tasks: ScheduledTasksRx,
    /// Provides latest blockhash and slot for building transactions.
    block: LatestBlock,
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
        rpc_url: String,
        faucet: Keypair,
        scheduled_tasks: ScheduledTasksRx,
        block: LatestBlock,
        slot_interval: tokio::time::Duration,
        token: CancellationToken,
    ) -> TaskSchedulerResult<Self> {
        Ok(Self {
            rpc_client: Arc::new(RpcClient::new(rpc_url)),
            faucet,
            scheduled_tasks,
            block,
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
                Some(request) = self.scheduled_tasks.recv() => {
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
    async fn crank_exists(&self, crank: &Pubkey) -> bool {
        matches!(
            self.rpc_client.get_account(crank).await,
            Ok(account) if account.owner == EPHEMERAL_PROGRAM_ID
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
    ) -> TaskSchedulerResult<Signature> {
        let crank = crank_pubkey(authority, task_id);
        let crank_exists = self.crank_exists(&crank).await;

        let snapshot = self.block.load();
        let start_slot = snapshot.slot;
        let blockhash = snapshot.blockhash;

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

        let validator = validator_authority();
        let ixs = if crank_exists {
            let cancel_ix =
                ephemeral::cancel(faucet_pubkey, crank, faucet_pubkey);
            vec![cancel_ix, create_ix, fund_ix]
        } else {
            vec![create_ix, fund_ix]
        };
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&validator_authority_id()),
            &[&validator, &self.faucet],
            blockhash,
        );

        // Confirm before returning so successive cranks (all sponsored by the
        // same faucet) don't race on the faucet's delegated account state.
        self.rpc_client
            .send_and_confirm_transaction(&tx)
            .await
            .map_err(Box::new)
            .map_err(TaskSchedulerError::from)
    }

    /// Sends and confirms a crank cancellation. The crank's remaining lamports
    /// are returned to the faucet (the cancel recipient).
    async fn send_cancel(
        &self,
        crank: Pubkey,
    ) -> TaskSchedulerResult<Signature> {
        let faucet_pubkey = self.faucet.pubkey();
        let blockhash = self.block.load().blockhash;
        let cancel_ix = ephemeral::cancel(faucet_pubkey, crank, faucet_pubkey);
        let validator = validator_authority();
        let tx = Transaction::new_signed_with_payer(
            &[cancel_ix],
            Some(&validator_authority_id()),
            &[&validator, &self.faucet],
            blockhash,
        );
        self.rpc_client
            .send_and_confirm_transaction(&tx)
            .await
            .map_err(Box::new)
            .map_err(TaskSchedulerError::from)
    }
}

#[cfg(test)]
mod tests {
    use magicblock_core::coordination_mode::switch_to_primary_mode;
    use serial_test::serial;
    use tokio::sync::mpsc;

    use super::*;

    fn test_service(scheduled_tasks: ScheduledTasksRx) -> TaskSchedulerService {
        TaskSchedulerService {
            rpc_client: Arc::new(RpcClient::new(
                "http://localhost:8899".to_string(),
            )),
            faucet: Keypair::new(),
            block: LatestBlock::default(),
            token: CancellationToken::new(),
            slot_interval: tokio::time::Duration::from_millis(1000),
            scheduled_tasks,
        }
    }

    #[serial]
    #[tokio::test]
    async fn test_service_shuts_down_on_cancel() {
        magicblock_core::logger::init_for_tests();
        switch_to_primary_mode();

        let (_tx, rx) = mpsc::unbounded_channel();
        let service = test_service(rx);
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
