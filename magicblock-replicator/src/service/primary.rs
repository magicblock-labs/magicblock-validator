//! Primary node: publishes events and holds leader lock.

use std::{collections::VecDeque, time::Duration};

use magicblock_core::link::replication::Message;
use tokio::{
    sync::mpsc::Receiver,
    time::{timeout, Instant},
};
use tracing::{error, info, instrument, warn};

use super::{ReplicationContext, LOCK_REFRESH_INTERVAL};
use crate::{
    nats::{Confirm, PendingPublish, Producer, Subjects},
    watcher::SnapshotWatcher,
    Result,
};

const LOCK_RETRY_WARN_INTERVAL: Duration = Duration::from_secs(5);
const PUBLISH_RETRY_BASE_DELAY: Duration = Duration::from_millis(50);
const PUBLISH_RETRY_MAX_DELAY: Duration = Duration::from_secs(1);
const PUBLISH_RETRY_LIMIT: usize = 5;
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);
// Stay below async-nats' configured in-flight acknowledgement limit so an
// unusually busy slot cannot block publishing before its Block fence arrives.
const MAX_DEFERRED_TRANSACTION_ACKS: usize = 1024;

/// Primary node: publishes events and holds leader lock.
pub struct Primary {
    pub(crate) ctx: ReplicationContext,
    producer: Producer,
    messages: Receiver<Message>,
    snapshots: SnapshotWatcher,
    unconfirmed_transactions: VecDeque<PendingTransaction>,
}

struct PendingTransaction {
    message: Message,
    ack: Option<PendingPublish>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LockState {
    Held,
    Reacquiring { last_warned_at: Option<Instant> },
}

impl LockState {
    fn can_publish(self) -> bool {
        matches!(self, Self::Held)
    }

    fn on_refresh_lost(&mut self) {
        *self = Self::Reacquiring {
            last_warned_at: None,
        };
    }

    fn on_reacquire_result(&mut self, acquired: bool) {
        if acquired {
            *self = Self::Held;
        }
    }

    fn should_warn(&mut self, now: Instant) -> bool {
        let Self::Reacquiring { last_warned_at } = self else {
            return true;
        };

        let should_warn = last_warned_at
            .map(|last| now.duration_since(last) >= LOCK_RETRY_WARN_INTERVAL)
            .unwrap_or(true);
        if should_warn {
            *last_warned_at = Some(now);
        }
        should_warn
    }
}

impl Primary {
    /// Creates a new primary instance.
    pub fn new(
        ctx: ReplicationContext,
        producer: Producer,
        messages: Receiver<Message>,
        snapshots: SnapshotWatcher,
    ) -> Self {
        Self {
            ctx,
            producer,
            messages,
            snapshots,
            unconfirmed_transactions: VecDeque::new(),
        }
    }

    /// Runs the state replication until shutdown.
    #[instrument(skip(self))]
    pub async fn run(mut self) {
        info!("entering primary replication mode");
        let mut lock_tick = tokio::time::interval(LOCK_REFRESH_INTERVAL);
        let mut lock_state = LockState::Held;
        let mut pending = None;

        loop {
            tokio::select! {
                biased;
                _ = self.ctx.cancel.cancelled() => {
                    self.drain_on_shutdown(&mut pending, lock_state).await;
                    self.release_producer_lock(lock_state).await;
                    return;
                }
                _ = async {}, if lock_state.can_publish() && pending.is_some() => {
                    if let Some(msg) = pending.as_ref() {
                        if self.publish_with_retry(msg).await {
                            pending = None;
                        }
                    }
                }
                _ = lock_tick.tick() => {
                    match lock_state {
                        LockState::Held => match self.producer.refresh().await {
                            Ok(true) => {}
                            Ok(false) => {
                                warn!("primary lock lost, waiting to reacquire");
                                lock_state.on_refresh_lost();
                            }
                            Err(e) => {
                                warn!(%e, "lock refresh failed, waiting to reacquire");
                                lock_state.on_refresh_lost();
                            }
                        },
                        LockState::Reacquiring { .. } => match self.producer.acquire().await {
                            Ok(true) => {
                                info!("primary lock reacquired");
                                lock_state.on_reacquire_result(true);
                            }
                            Ok(false) => {
                                if lock_state.should_warn(Instant::now()) {
                                    warn!("primary lock still unavailable, retrying");
                                }
                            }
                            Err(e) => {
                                if lock_state.should_warn(Instant::now()) {
                                    warn!(%e, "failed to reacquire primary lock");
                                }
                            }
                        }
                    }
                }
                msg = self.messages.recv(), if pending.is_none() => {
                    let Some(msg) = msg else {
                        self.release_producer_lock(lock_state).await;
                        return;
                    };
                    if lock_state.can_publish() {
                        if !self.publish_with_retry(&msg).await {
                            pending = Some(msg);
                        }
                    } else {
                        pending = Some(msg);
                    }
                }
                snapshot = self.snapshots.recv() => {
                    if let Some((file, slot)) = snapshot {
                        if let Err(e) = self.ctx.upload_snapshot(file, slot).await {
                            warn!(%e, "snapshot upload failed");
                        }
                    }
                }
            }
        }
    }

    async fn drain_on_shutdown(
        &mut self,
        pending: &mut Option<Message>,
        lock_state: LockState,
    ) {
        info!(
            timeout_ms = SHUTDOWN_DRAIN_TIMEOUT.as_millis() as u64,
            "shutdown received, draining replication messages"
        );

        if !lock_state.can_publish() {
            warn!("primary lock is not held, skipping replication drain");
            return;
        }
        let _timing = ShutdownTiming::new("replication_service_drain");

        let deadline = Instant::now() + SHUTDOWN_DRAIN_TIMEOUT;
        // Transactions remain pipelined while the producer finishes. Block
        // fences and the final flush below still guarantee that a successful
        // drain has confirmed every message without paying one NATS round trip
        // per queued transaction.
        let mut drained = 0_u64;

        if let Some(msg) = pending.take() {
            if !self.drain_publish(&msg, deadline).await {
                warn!(drained, "failed to drain pending replication message");
                return;
            }
            drained += 1;
        }

        loop {
            match next_to_drain(&mut self.messages, deadline).await {
                DrainNext::Publish(msg) => {
                    if !self.drain_publish(&msg, deadline).await {
                        warn!(
                            drained,
                            "failed to drain queued replication message"
                        );
                        return;
                    }
                    drained += 1;
                }
                DrainNext::ProducerFinished => {
                    if !self.confirm_transactions(Some(deadline)).await {
                        warn!(
                            drained,
                            "failed to confirm drained replication messages"
                        );
                        return;
                    }
                    info!(drained, "replication shutdown drain complete");
                    return;
                }
                DrainNext::TimedOut => {
                    warn!(drained, "replication shutdown drain timed out");
                    return;
                }
            }
        }
    }

    /// Publishes during shutdown using the normal ordering fences.
    async fn drain_publish(
        &mut self,
        msg: &Message,
        deadline: Instant,
    ) -> bool {
        self.publish_ordered_until(msg, Some(deadline)).await
    }

    async fn release_producer_lock(&self, lock_state: LockState) {
        if !lock_state.can_publish() {
            return;
        }
        let _timing = ShutdownTiming::new("replication_service_lock_release");
        if let Err(error) = self.producer.release().await {
            warn!(%error, "failed to release the lock");
        }
    }

    /// Publishes `msg`, waiting for the server as far as `confirm` demands.
    async fn publish(&mut self, msg: &Message, confirm: Confirm) -> Result<()> {
        let payload = match bincode::serialize(msg) {
            Ok(p) => p,
            Err(error) => {
                error!(%error, "serialization failed, should never happen");
                return Ok(());
            }
        };
        let subject = Subjects::from_message(msg);
        let (slot, index) = msg.slot_and_index();
        let msg_id = message_id(slot, index);

        let payload = payload.into();
        let ack = if confirm == Confirm::No {
            Some(
                self.ctx
                    .broker
                    .publish_deferred(subject, payload, Some(msg_id.as_str()))
                    .await?,
            )
        } else {
            self.ctx
                .broker
                .publish(subject, payload, Some(msg_id.as_str()), confirm)
                .await?;
            None
        };
        if let Some(ack) = ack {
            self.unconfirmed_transactions.push_back(PendingTransaction {
                message: msg.clone(),
                ack: Some(ack),
            });
        }
        self.ctx.update_position(slot, index);
        Ok(())
    }

    async fn publish_with_retry(&mut self, msg: &Message) -> bool {
        self.publish_ordered_until(msg, None).await
    }

    async fn publish_ordered_until(
        &mut self,
        msg: &Message,
        deadline: Option<Instant>,
    ) -> bool {
        let fence = matches!(msg, Message::Block(_))
            || (matches!(msg, Message::Transaction(_))
                && self.unconfirmed_transactions.len()
                    >= MAX_DEFERRED_TRANSACTION_ACKS);
        if fence && !self.confirm_transactions(deadline).await {
            return false;
        }
        self.publish_with_retry_until(msg, deadline, confirmation_for(msg))
            .await
    }

    /// Confirms every transaction published before the next block boundary.
    /// Failed deferred publishes are retried with synchronous confirmation, so
    /// a block can never overtake a transaction that JetStream did not persist.
    async fn confirm_transactions(
        &mut self,
        deadline: Option<Instant>,
    ) -> bool {
        while let Some(mut pending) = self.unconfirmed_transactions.pop_front()
        {
            let confirmed = if let Some(ack) = pending.ack.take() {
                match deadline {
                    Some(deadline) => {
                        let Some(remaining) = remaining_until(deadline) else {
                            self.unconfirmed_transactions.push_front(pending);
                            return false;
                        };
                        match timeout(
                            remaining,
                            self.ctx.broker.confirm(ack, false),
                        )
                        .await
                        {
                            Ok(Ok(())) => true,
                            Ok(Err(error)) => {
                                warn!(%error, "transaction publish was not confirmed, retrying");
                                false
                            }
                            Err(_) => {
                                warn!("timed out confirming a transaction publish");
                                false
                            }
                        }
                    }
                    None => match self.ctx.broker.confirm(ack, false).await {
                        Ok(()) => true,
                        Err(error) => {
                            warn!(%error, "transaction publish was not confirmed, retrying");
                            false
                        }
                    },
                }
            } else {
                false
            };

            if confirmed {
                continue;
            }
            if self
                .publish_with_retry_until(
                    &pending.message,
                    deadline,
                    Confirm::Yes,
                )
                .await
            {
                continue;
            }

            self.unconfirmed_transactions.push_front(pending);
            return false;
        }
        true
    }

    async fn publish_with_retry_until(
        &mut self,
        msg: &Message,
        deadline: Option<Instant>,
        confirm: Confirm,
    ) -> bool {
        let mut delay = PUBLISH_RETRY_BASE_DELAY;

        for attempt in 0..PUBLISH_RETRY_LIMIT {
            if deadline.is_none() && self.ctx.cancel.is_cancelled() {
                return false;
            }

            let result = match deadline {
                Some(deadline) => {
                    let Some(remaining) = remaining_until(deadline) else {
                        return false;
                    };
                    match timeout(remaining, self.publish(msg, confirm)).await {
                        Ok(result) => result,
                        Err(_) => {
                            warn!("timed out publishing the message");
                            return false;
                        }
                    }
                }
                None => self.publish(msg, confirm).await,
            };

            match result {
                Ok(()) => return true,
                Err(error) => {
                    warn!(
                        %error,
                        attempt = attempt + 1,
                        max_attempts = PUBLISH_RETRY_LIMIT,
                        "failed to publish the message"
                    );
                }
            }

            if attempt + 1 == PUBLISH_RETRY_LIMIT {
                break;
            }

            if let Some(deadline) = deadline {
                let Some(remaining) = remaining_until(deadline) else {
                    return false;
                };
                if remaining <= delay {
                    return false;
                }
            }

            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(PUBLISH_RETRY_MAX_DELAY);
        }

        false
    }
}

struct ShutdownTiming {
    step: &'static str,
    start: Instant,
}

impl ShutdownTiming {
    fn new(step: &'static str) -> Self {
        Self {
            step,
            start: Instant::now(),
        }
    }
}

impl Drop for ShutdownTiming {
    fn drop(&mut self) {
        info!(
            phase = "shutdown",
            step = self.step,
            duration_ms = self.start.elapsed().as_millis() as u64,
            "Validator timing"
        );
    }
}

fn remaining_until(deadline: Instant) -> Option<Duration> {
    let now = Instant::now();
    (now < deadline).then_some(deadline - now)
}

fn message_id(slot: u64, index: u32) -> String {
    format!("{slot}:{index}")
}

/// The next step of a shutdown drain.
#[derive(Debug)]
pub(crate) enum DrainNext {
    Publish(Message),
    /// Every sender is gone: the producer has finished and nothing more is coming.
    ProducerFinished,
    TimedOut,
}

/// Waits for the next message to drain.
///
/// Waits for the producer to *finish*, not merely to go quiet. The scheduler and
/// this service observe the same shutdown signal, and the scheduler still has to
/// assemble its final block — awaiting executors, a ledger write, sometimes a
/// snapshot — before sending it. An empty channel at this moment means only that
/// it has not got there yet, so treating empty as done drops that block, and
/// `send_replication` finds the receiver gone and discards it.
pub(crate) async fn next_to_drain(
    messages: &mut Receiver<Message>,
    deadline: Instant,
) -> DrainNext {
    let Some(remaining) = remaining_until(deadline) else {
        return DrainNext::TimedOut;
    };
    match timeout(remaining, messages.recv()).await {
        Ok(Some(msg)) => DrainNext::Publish(msg),
        Ok(None) => DrainNext::ProducerFinished,
        Err(_) => DrainNext::TimedOut,
    }
}

/// How far each kind of message waits for the server.
///
/// Transaction acknowledgements are deferred and drained at the next Block, so
/// the hot path pipelines publishes without allowing a persisted block boundary
/// to overtake an unconfirmed transaction.
pub(crate) fn confirmation_for(msg: &Message) -> Confirm {
    match msg {
        // Hundreds per second: pipeline acknowledgements until the Block fence.
        Message::Transaction(_) => Confirm::No,
        // A few per second, and a lost one is near-silent: nothing goes
        // unapplied, so no checksum trips, but replicas seed the next slot from
        // the wrong blockhash.
        Message::Block(_) => Confirm::Yes,
        // Rare, and they double as a consumer's resume point after a snapshot.
        Message::SuperBlock(_) | Message::Reset(_) => Confirm::AndTrackSequence,
    }
}
