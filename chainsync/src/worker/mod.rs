mod coverage;
mod lifecycle;
mod operations;
mod scheduler;

use self::{
    coverage::{Coverage, Source, Ticket, UpdateSource},
    operations::{Completion, Operations, Update, Work},
    scheduler::{Scheduler, WORK_LIMIT},
};
use crate::{
    AccountSubscription, ChainSync, Result, ata,
    grpc::{self, Command},
    websocket,
};
use futures::{StreamExt, stream::FuturesUnordered};
use nucleus::shutdown::{ShutdownHandle, ShutdownReason};
use solana_account::AccountBuilder;
use solana_pubkey::Pubkey;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Arc,
};
use tokio::sync::{
    mpsc::{self, Receiver, UnboundedReceiver},
    oneshot,
};
use tracing::error;

/// Coverage and scheduling are owned by this loop, never by futures waiting on Engine or providers.
struct Worker<'a> {
    sync: &'a ChainSync,
    coverage: Coverage,
    scheduler: Scheduler,
}

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error("ChainSync worker capacity exhausted ({limit} outstanding operations)")]
    Capacity { limit: usize },
    #[error(transparent)]
    Operation(#[from] crate::Error),
}

type RunResult<T> = std::result::Result<T, RunError>;

/// Only running operations produce controls, and each waits for its reply before issuing another.
enum Control {
    Retire {
        pubkey: Pubkey,
        reply: oneshot::Sender<Result<()>>,
    },
    Authorize {
        ticket: Ticket,
        reply: oneshot::Sender<Result<bool>>,
    },
    Covered {
        pubkey: Pubkey,
        reply: oneshot::Sender<Result<bool>>,
    },
}

impl ChainSync {
    /// Runs the update worker until coordinated shutdown or a terminal processing failure.
    /// Drops the worker's shared handle before reporting its termination reason.
    pub(super) async fn run(
        self: Arc<Self>,
        websocket: Receiver<websocket::Event>,
        grpc: Receiver<grpc::Event>,
        evictions: UnboundedReceiver<Pubkey>,
        mut shutdown: ShutdownHandle,
    ) {
        let reason = {
            let mut worker = Worker::new(&self);
            tokio::select! {
                biased;
                _ = shutdown.signalled() => ShutdownReason::Signalled,
                result = worker.run_updates(websocket, grpc, evictions) => match result {
                    Ok(()) => {
                        error!("ChainSync worker stopped unexpectedly");
                        ShutdownReason::Unexpected
                    }
                    Err(error) => {
                        error!(%error, "ChainSync worker stopped unexpectedly");
                        ShutdownReason::Error(Box::new(error))
                    }
                },
            }
        };
        drop(self);
        shutdown.terminate(reason);
    }
}

impl<'a> Worker<'a> {
    fn new(sync: &'a ChainSync) -> Self {
        Self {
            sync,
            coverage: Coverage::default(),
            scheduler: Scheduler::new(WORK_LIMIT),
        }
    }

    fn grpc_client(&self, pubkey: Pubkey) -> &grpc::Client {
        let mut hash = DefaultHasher::new();
        pubkey.hash(&mut hash);
        let index = hash.finish() as usize % self.sync.grpc.len();
        &self.sync.grpc[index]
    }

    /// All branches are nonblocking: Engine and provider waits belong to operation futures.
    async fn run_updates(
        &mut self,
        mut websocket: Receiver<websocket::Event>,
        mut grpc: Receiver<grpc::Event>,
        mut evictions: UnboundedReceiver<Pubkey>,
    ) -> RunResult<()> {
        let (controls, mut requests) = mpsc::unbounded_channel();
        let operations = Operations { sync: self.sync, controls };
        let mut running = FuturesUnordered::new();
        loop {
            while let Some(work) = self.scheduler.next() {
                running.push(operations.execute(work));
            }
            // No source has priority over control replies or completions under sustained traffic.
            tokio::select! {
                Some(event) = websocket.recv() => self.on_websocket(event)?,
                Some(event) = grpc.recv() => self.on_grpc(event)?,
                Some(request) = requests.recv() => self.control(request),
                Some(completion) = running.next(), if !running.is_empty() => self.complete(completion)?,
                pubkey = evictions.recv() => {
                    let Some(pubkey) = pubkey else { return Ok(()) };
                    self.scheduler.admit(Work::Evict { pubkey, cached: true })?;
                }
            }
        }
    }

    fn complete(&mut self, completion: Completion) -> RunResult<()> {
        self.scheduler.finish(completion.lane);
        if completion.fatal {
            completion.result?;
        }
        Ok(())
    }

    fn control(&mut self, control: Control) {
        match control {
            Control::Retire { pubkey, reply } => {
                let _ = reply.send(self.unsubscribe_account(pubkey));
            }
            Control::Authorize { ticket, reply } => {
                let _ = reply.send(Ok(self.coverage.authorizes(ticket)));
            }
            Control::Covered { pubkey, reply } => {
                let _ = reply.send(Ok(self.coverage.covers(pubkey)));
            }
        }
    }

    /// Revoke coverage before acknowledging retirement to an operation holding an Engine lease.
    fn unsubscribe_account(&mut self, pubkey: Pubkey) -> Result<()> {
        for sub in AccountSubscription::for_account(pubkey) {
            if self.coverage.remove(sub) {
                self.grpc_client(sub.pubkey).command(Command::Remove(sub.pubkey));
            }
        }
        self.sync.websocket.unsubscribe_account(pubkey)?;
        Ok(())
    }

    fn update(
        &mut self,
        source: UpdateSource,
        sub: AccountSubscription,
        account: AccountBuilder,
    ) -> RunResult<()> {
        if let Some(ticket) = self.coverage.ticket(source, sub) {
            let update = if account.read().owner() == delegation_program_api::id()
                && ata::is_raw_eata(sub.pubkey, &account)
            {
                Update::Refresh(account.read().slot())
            } else {
                Update::Mirror(account)
            };
            self.scheduler.admit(Work::Update { ticket, update })?;
        }
        Ok(())
    }

    fn on_websocket(&mut self, event: websocket::Event) -> RunResult<()> {
        match event {
            websocket::Event::Acknowledged(sub) => {
                let generation = self.coverage.acknowledged(sub);
                self.grpc_client(sub.pubkey).command(Command::Track { sub, generation });
            }
            websocket::Event::Removed(pubkey) => {
                // Explicit release revokes coverage; the caller, not a delayed event, owns cleanup.
                self.coverage.revoke(pubkey, None);
                self.grpc_client(pubkey).command(Command::Remove(pubkey));
            }
            websocket::Event::Update { sub, account } => {
                self.update(UpdateSource::WebSocket, sub, account)?;
            }
            websocket::Event::Dropped { pubkeys } => {
                for pubkey in pubkeys {
                    self.lost(Source::WebSocket, pubkey)?;
                }
            }
        }
        Ok(())
    }

    fn lost(&mut self, source: Source, pubkey: Pubkey) -> RunResult<()> {
        let (ended, local_pubkey) = self.coverage.lost(source, pubkey);
        if ended {
            self.grpc_client(pubkey).command(Command::Remove(pubkey));
        }
        if let Some(pubkey) = local_pubkey {
            self.scheduler.admit(Work::Evict { pubkey, cached: false })?;
        }
        Ok(())
    }

    fn on_grpc(&mut self, event: grpc::Event) -> RunResult<()> {
        match event {
            grpc::Event::Confirmed { stream, pubkey, generation } => {
                self.coverage.confirmed(stream, pubkey, generation);
            }
            grpc::Event::Lost(stream) => {
                for pubkey in self.coverage.stream_keys(stream) {
                    self.lost(Source::Grpc, pubkey)?;
                }
            }
            grpc::Event::Update { stream, sub, account } => {
                self.update(UpdateSource::Grpc(stream), sub, account)?;
            }
            grpc::Event::Delegated(delegation) => {
                self.scheduler.admit(Work::Delegated(delegation))?
            }
            grpc::Event::UndelegationRequested { pubkey, slot } => {
                self.scheduler.admit(Work::UndelegationRequested { pubkey, slot })?;
            }
            grpc::Event::Undelegated { pubkeys, slot } => {
                for pubkey in pubkeys {
                    self.scheduler.admit(Work::Undelegated { pubkey, slot })?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
