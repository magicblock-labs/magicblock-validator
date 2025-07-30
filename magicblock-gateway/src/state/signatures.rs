use std::{
    collections::VecDeque,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use solana_signature::Signature;
use tokio::time::{self, Interval};

pub(crate) struct SignaturesExpirer {
    pub(crate) cache: VecDeque<ExpiringSignature>,
    tick: u64,
    ticker: Interval,
}

pub(crate) struct ExpiringSignature {
    ttl: u64,
    pub(crate) signature: Signature,
    subscribed: Arc<AtomicBool>,
}

impl SignaturesExpirer {
    const WAIT: u64 = 5;
    const TTL: u64 = 90 / Self::WAIT;
    pub(crate) fn init() -> Self {
        Self {
            cache: Default::default(),
            tick: 0,
            ticker: time::interval(Duration::from_secs(Self::WAIT)),
        }
    }

    pub(crate) fn push(
        &mut self,
        signature: Signature,
        subscribed: Arc<AtomicBool>,
    ) {
        let sig = ExpiringSignature {
            signature,
            ttl: self.tick + Self::TTL,
            subscribed,
        };
        self.cache.push_back(sig);
    }

    pub(crate) async fn step(&mut self) -> Signature {
        loop {
            'expire: {
                let Some(s) = self.cache.front() else {
                    break 'expire;
                };
                if s.ttl > self.tick {
                    break 'expire;
                }
                let Some(s) = self.cache.pop_front() else {
                    break 'expire;
                };
                if s.subscribed.load(std::sync::atomic::Ordering::Relaxed) {
                    return s.signature;
                }
            }
            self.ticker.tick().await;
            self.tick += 1;
        }
    }
}
