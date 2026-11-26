//! FIFO mutation lanes with global barriers for work that can touch multiple accounts.

use std::{
    collections::{VecDeque, hash_map::Entry},
    mem,
};

use super::{RunError, Work};
use ahash::AHashMap;
use solana_pubkey::Pubkey;

pub(super) const WORK_LIMIT: usize = 1 << 13;

enum Phase {
    Open,
    /// Earlier lanes must finish before this barrier can start.
    Draining(Work),
    /// The barrier is ready or running.
    Barrier,
}

pub(super) struct Scheduler {
    /// An entry exists while its head runs; the queue holds only subsequent operations.
    lanes: AHashMap<Pubkey, VecDeque<Work>>,
    ready: VecDeque<Work>,
    /// Work admitted after the current barrier.
    deferred: VecDeque<Work>,
    phase: Phase,
    /// Includes ready, queued, and running operations; controls and continuations do not add slots.
    pub(super) outstanding: usize,
    limit: usize,
}

impl Scheduler {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            lanes: AHashMap::new(),
            ready: VecDeque::new(),
            deferred: VecDeque::new(),
            phase: Phase::Open,
            outstanding: 0,
            limit,
        }
    }

    pub(super) fn admit(&mut self, work: Work) -> Result<(), RunError> {
        if self.outstanding == self.limit {
            return Err(RunError::Capacity { limit: self.limit });
        }
        self.outstanding += 1;
        if matches!(self.phase, Phase::Open) {
            self.queue_open(work);
        } else {
            self.deferred.push_back(work);
        }
        Ok(())
    }

    fn queue_open(&mut self, work: Work) {
        match work.lane() {
            Some(key) => match self.lanes.entry(key) {
                Entry::Occupied(mut lane) => lane.get_mut().push_back(work),
                Entry::Vacant(lane) => {
                    lane.insert(VecDeque::new());
                    self.ready.push_back(work);
                }
            },
            None if self.lanes.is_empty() => {
                self.phase = Phase::Barrier;
                self.ready.push_back(work);
            }
            None => self.phase = Phase::Draining(work),
        }
    }

    pub(super) fn next(&mut self) -> Option<Work> {
        self.ready.pop_front()
    }

    /// A completion releases exactly one slot, then promotes only work before the next barrier.
    pub(super) fn finish(&mut self, lane: Option<Pubkey>) {
        self.outstanding -= 1;
        if let Some(key) = lane {
            if let Some(next) = self.lanes.get_mut(&key).and_then(VecDeque::pop_front) {
                self.ready.push_back(next);
                return;
            }
            self.lanes.remove(&key);
            if self.lanes.is_empty()
                && let Phase::Draining(barrier) = mem::replace(&mut self.phase, Phase::Open)
            {
                self.phase = Phase::Barrier;
                self.ready.push_back(barrier);
            }
            return;
        }
        self.phase = Phase::Open;
        while matches!(self.phase, Phase::Open)
            && let Some(work) = self.deferred.pop_front()
        {
            self.queue_open(work);
        }
    }
}
