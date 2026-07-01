use std::{path::Path, sync::Arc, time::Duration as StdDuration};

use chrono::Utc;
use hydra_api::{
    consts::{CRANKER_REWARD, CRANK_HEADER_SIZE, SERIALIZED_META_SIZE},
    ephemeral::ID as EPHEMERAL_PROGRAM_ID,
    instruction::{ephemeral, CreateArgs, SchedMeta, ScheduledIx},
};
use magicblock_core::link::transactions::ScheduledTasksRx;
use magicblock_ledger::LatestBlock;
use magicblock_program::{
    args::{CancelTaskRequest, ScheduleTaskRequest, TaskRequest},
    validator::{validator_authority, validator_authority_id},
    EPHEMERAL_RENT_PER_BYTE,
};
use solana_account::AccountSharedData;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tokio::{
    select,
    task::JoinHandle,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use tracing::*;

use crate::{
    db::{DbTask, SchedulerDatabase},
    errors::{TaskSchedulerError, TaskSchedulerResult},
};

/// How long migration waits for the validator to produce a usable blockhash
/// before giving up.
const BLOCK_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the scheduler waits for the faucet to be delegated and funded in
/// the ephemeral rollup before it starts paying for cranks.
const FAUCET_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// The task scheduler migrates any tasks persisted by the legacy
/// (validator-funded) scheduler onto the hydra crank program at startup, then
/// serves runtime schedule/cancel requests by sending hydra transactions. The
/// SQLite database is used solely for that one-time migration; the runtime path
/// is stateless and derives each task's crank PDA deterministically from
/// `(authority, task_id)`.
pub struct TaskSchedulerService {
    /// Migration-only database of legacy tasks.
    db: SchedulerDatabase,
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
    slot_interval: Duration,
}

// SAFETY: TaskSchedulerService is moved into a single Tokio task in `start()`
// and never cloned. It runs exclusively on that task. All fields are Send+Sync.
unsafe impl Send for TaskSchedulerService {}
unsafe impl Sync for TaskSchedulerService {}
impl TaskSchedulerService {
    /// Creates a new `TaskSchedulerService`.
    pub fn new(
        path: &Path,
        rpc_url: String,
        faucet: Option<Keypair>,
        scheduled_tasks: ScheduledTasksRx,
        block: LatestBlock,
        slot_interval: Duration,
        token: CancellationToken,
    ) -> TaskSchedulerResult<Self> {
        let Some(faucet) = faucet else {
            warn!("No faucet keypair configured, skipping task scheduler");
            return Err(TaskSchedulerError::FaucetNotReady);
        };
        let db = SchedulerDatabase::new(path)?;
        Ok(Self {
            db,
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

    /// Main loop: migrate persisted tasks once, then serve runtime requests.
    async fn run(mut self) -> TaskSchedulerResult<()> {
        if let Err(e) = self.migrate_persisted_tasks().await {
            error!("Task migration failed: {}", e);
        }

        // Ensure the faucet is funded before serving runtime requests so the
        // first scheduled task is not dropped for lack of a payer.
        self.wait_for_faucet_ready().await?;

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

    /// Migrates every task persisted by the legacy scheduler onto hydra, then
    /// empties the database. Invalid tasks are dropped without rescheduling.
    /// Migration is best-effort: a task is removed from the database whether or
    /// not its hydra crank could be created, so the database always empties.
    async fn migrate_persisted_tasks(&self) -> TaskSchedulerResult<()> {
        let tasks = self.db.get_tasks().await?;
        if tasks.is_empty() {
            return Ok(());
        }
        info!("Migrating {} persisted task(s) onto hydra", tasks.len());

        // Drop tasks that can no longer correspond to a live crank without
        // touching the network.
        let (valid, invalid): (Vec<DbTask>, Vec<DbTask>) =
            tasks.into_iter().partition(|task| {
                is_valid_task_interval(task.execution_interval_millis)
                    && task.executions_left > 0
            });
        for task in invalid {
            warn!(
                "Dropping invalid task {} during migration (interval={}, executions_left={})",
                task.id, task.execution_interval_millis, task.executions_left
            );
            self.db.remove_task(task.id).await?;
        }

        if valid.is_empty() {
            return Ok(());
        }

        // Sending a crank create needs a usable blockhash and a funded faucet
        // (delegated and cloned into the ephemeral rollup) to pay with.
        self.wait_for_block_ready().await;
        info!("Migration: block ready");
        self.wait_for_faucet_ready().await?;
        info!("Migration: faucet ready");

        for task in valid {
            info!("Migration: creating crank for task {}", task.id);
            match self
                .schedule_crank(
                    &task.authority,
                    task.id,
                    task.execution_interval_millis,
                    task.executions_left,
                    &task.instructions,
                    Some(task.last_execution_millis),
                )
                .await
            {
                Ok(()) => {
                    self.db.remove_task(task.id).await?;
                    debug!("Migration: created crank for task {}", task.id)
                }
                Err(e) => {
                    warn!(
                        "Failed to migrate task {} onto hydra: {}",
                        task.id, e
                    )
                }
            }
        }

        info!("Task migration complete");
        Ok(())
    }

    /// Waits until the faucet has a non-zero balance in the ephemeral rollup
    /// (i.e. it has been delegated on the base chain and cloned in), or the
    /// scheduler is cancelled, or [`FAUCET_READY_TIMEOUT`] elapses.
    async fn wait_for_faucet_ready(&self) -> TaskSchedulerResult<()> {
        let faucet = self.faucet.pubkey();
        let start = Instant::now();
        while start.elapsed() < FAUCET_READY_TIMEOUT {
            if self.token.is_cancelled() {
                return Ok(());
            }
            if matches!(
                self.rpc_client.get_account(&faucet).await,
                Ok(account) if account.lamports > 0
            ) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        warn!(%faucet, "Timed out waiting for faucet to be delegated before paying cranks");
        Err(TaskSchedulerError::FaucetNotReady)
    }

    /// Waits until the validator has a usable blockhash, or the scheduler is
    /// cancelled, or [`BLOCK_READY_TIMEOUT`] elapses.
    async fn wait_for_block_ready(&self) {
        let start = Instant::now();
        while start.elapsed() < BLOCK_READY_TIMEOUT {
            if self.token.is_cancelled()
                || self.block.load().blockhash != Default::default()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        warn!("Timed out waiting for a usable blockhash before migration");
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
            None,
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
        last_execution_millis: Option<i64>,
    ) -> TaskSchedulerResult<()> {
        self.send_create(
            authority,
            task_id,
            interval_millis,
            iterations,
            instructions,
            last_execution_millis,
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
        last_execution_millis: Option<i64>,
    ) -> TaskSchedulerResult<Signature> {
        let crank = crank_pubkey(authority, task_id);
        let crank_exists = self.crank_exists(&crank).await;

        let snapshot = self.block.load();
        let start_slot = last_execution_millis
            .map(|last_execution_millis| {
                legacy_start_slot(
                    last_execution_millis,
                    interval_millis,
                    Utc::now().timestamp_millis(),
                    snapshot.slot,
                    self.slot_interval,
                )
            })
            .unwrap_or(snapshot.slot);
        let blockhash = snapshot.blockhash;

        let interval_slots =
            interval_slots(interval_millis, self.slot_interval);
        let iterations = iterations.max(0) as u64;

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

/// Derives the deterministic hydra crank account address for a task.
///
/// The seed is `hash(authority, task_id)`, so each authority gets its own crank
/// namespace: a different authority scheduling the same `task_id` gets an
/// independent crank, and cancel/reschedule need no database lookup.
pub fn crank_pubkey(authority: &Pubkey, task_id: i64) -> Pubkey {
    let seed = solana_sha256_hasher::hashv(&[
        authority.as_ref(),
        &task_id.to_le_bytes(),
    ])
    .to_bytes();
    ephemeral::find_crank_pda(&seed).0
}

/// Builds the hydra `Create` instruction embedding the task's instructions as
/// the scheduled crank payload. Account signer flags are intentionally dropped:
/// hydra rejects scheduled instructions that declare signers.
/// Rent-exempt minimum for the crank account hydra will allocate for these
/// scheduled instructions. Mirrors hydra's on-chain size accounting
/// (`CRANK_HEADER_SIZE + region_len`, where each scheduled ix serializes to
/// `2 + metas + program_id + 2 + data`).
fn crank_rent_floor(instructions: &[Instruction]) -> u64 {
    let region_len: usize = instructions
        .iter()
        .map(|ix| {
            2 + ix.accounts.len() * SERIALIZED_META_SIZE
                + 32
                + 2
                + ix.data.len()
        })
        .sum::<usize>()
        + CRANK_HEADER_SIZE;
    let total_size =
        (region_len + AccountSharedData::ACCOUNT_STATIC_SIZE as usize) as u64;
    total_size * EPHEMERAL_RENT_PER_BYTE
}

#[allow(clippy::too_many_arguments)]
fn build_create_ix(
    faucet: &Pubkey,
    authority: &Pubkey,
    task_id: i64,
    crank: Pubkey,
    start_slot: u64,
    interval_slots: u64,
    iterations: u64,
    instructions: &[Instruction],
) -> Instruction {
    let seed = solana_sha256_hasher::hashv(&[
        authority.as_ref(),
        &task_id.to_le_bytes(),
    ])
    .to_bytes();

    let metas_per_ix: Vec<Vec<SchedMeta>> = instructions
        .iter()
        .map(|ix| {
            ix.accounts
                .iter()
                .map(|acc| SchedMeta {
                    pubkey: acc.pubkey.to_bytes(),
                    is_writable: acc.is_writable,
                })
                .collect()
        })
        .collect();

    let scheduled: Vec<ScheduledIx> = instructions
        .iter()
        .zip(metas_per_ix.iter())
        .map(|(ix, metas)| ScheduledIx {
            program_id: ix.program_id.to_bytes(),
            metas: metas.as_slice(),
            data: ix.data.as_slice(),
        })
        .collect();

    let args = CreateArgs {
        seed,
        // The faucet is the sponsor and the cancel authority for the crank.
        authority: faucet.to_bytes(),
        start_slot,
        interval_slots,
        remaining: iterations,
        priority_tip: 0,
        cu_limit: 0,
        scheduled: scheduled.as_slice(),
    };

    ephemeral::create(*faucet, crank, &args)
}

fn is_valid_task_interval(interval: i64) -> bool {
    interval > 0 && interval < u32::MAX as i64
}

/// Computes the hydra start slot for a task migrated from the legacy scheduler.
/// If the legacy task is overdue or has never run, it remains due immediately.
fn legacy_start_slot(
    last_execution_millis: i64,
    interval_millis: i64,
    current_millis: i64,
    current_slot: u64,
    slot_interval: StdDuration,
) -> u64 {
    if last_execution_millis <= 0 {
        return current_slot;
    }

    let next_execution_millis =
        last_execution_millis.saturating_add(interval_millis.max(0));
    let remaining_millis = next_execution_millis.saturating_sub(current_millis);
    if remaining_millis == 0 {
        return current_slot;
    }

    current_slot.saturating_add(interval_slots(remaining_millis, slot_interval))
}

/// Converts a millisecond execution interval into a slot count (rounding up,
/// with a one-slot minimum) for hydra's slot-based cadence.
fn interval_slots(interval_millis: i64, slot_interval: StdDuration) -> u64 {
    let slot_millis = (slot_interval.as_millis() as i64).max(1);
    let interval_millis = interval_millis.max(0);
    // Ceiling division without the unstable `i64::div_ceil`.
    let slots = (interval_millis + slot_millis - 1) / slot_millis;
    slots.max(1) as u64
}

#[cfg(test)]
mod tests {
    use magicblock_core::coordination_mode::switch_to_primary_mode;
    use serial_test::serial;
    use tokio::{sync::mpsc, time::timeout};

    use super::*;

    fn test_service(
        db: SchedulerDatabase,
        scheduled_tasks: ScheduledTasksRx,
    ) -> TaskSchedulerService {
        TaskSchedulerService {
            db,
            rpc_client: Arc::new(RpcClient::new(
                "http://localhost:8899".to_string(),
            )),
            faucet: Keypair::new(),
            block: LatestBlock::default(),
            token: CancellationToken::new(),
            slot_interval: Duration::from_millis(1000),
            scheduled_tasks,
        }
    }

    #[serial]
    #[test]
    fn test_interval_millis_rounds_up_to_slots() {
        let slot = StdDuration::from_millis(50);
        assert_eq!(interval_slots(1, slot), 1);
        assert_eq!(interval_slots(50, slot), 1);
        assert_eq!(interval_slots(51, slot), 2);
        assert_eq!(interval_slots(100, slot), 2);
    }

    #[serial]
    #[test]
    fn test_legacy_start_slot_preserves_remaining_delay() {
        let slot_interval = StdDuration::from_millis(1_000);

        assert_eq!(
            legacy_start_slot(10_000, 30_000, 25_000, 100, slot_interval),
            115
        );
        assert_eq!(
            legacy_start_slot(10_000, 30_000, 40_000, 100, slot_interval),
            100
        );
        assert_eq!(
            legacy_start_slot(0, 30_000, 25_000, 100, slot_interval),
            100
        );
    }

    #[serial]
    #[test]
    fn test_crank_pubkey_namespaced_by_authority_and_id() {
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        // Deterministic.
        assert_eq!(crank_pubkey(&a, 1), crank_pubkey(&a, 1));
        // Different task id -> different crank.
        assert_ne!(crank_pubkey(&a, 1), crank_pubkey(&a, 2));
        // Different authority, same id -> different crank (per-authority namespace).
        assert_ne!(crank_pubkey(&a, 1), crank_pubkey(&b, 1));
    }

    #[serial]
    #[tokio::test]
    async fn test_migration_drops_invalid_tasks_and_empties_db() {
        magicblock_core::logger::init_for_tests();
        switch_to_primary_mode();

        let (_tx, rx) = mpsc::unbounded_channel();
        let db = SchedulerDatabase::new(":memory:").unwrap();
        // Invalid interval.
        db.insert_task(&DbTask {
            id: 1,
            authority: Pubkey::new_unique(),
            execution_interval_millis: u32::MAX as i64,
            executions_left: 1,
            last_execution_millis: 0,
            instructions: vec![],
        })
        .await
        .unwrap();
        // Exhausted (no executions left).
        db.insert_task(&DbTask {
            id: 2,
            authority: Pubkey::new_unique(),
            execution_interval_millis: 50,
            executions_left: 0,
            last_execution_millis: 0,
            instructions: vec![],
        })
        .await
        .unwrap();

        let service = test_service(db.clone(), rx);
        let handle = service.start().await.unwrap();

        // Migration drops both invalid tasks without any network access, so the
        // database empties promptly.
        timeout(Duration::from_secs(2), async move {
            loop {
                if db.get_task_ids().await.unwrap().is_empty() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("database should empty after migration");

        handle.abort();
    }
}
