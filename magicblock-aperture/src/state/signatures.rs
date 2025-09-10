use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use solana_signature::Signature;
use tokio::time::{self, Interval};

/// Manages the lifecycle of `signatureSubscribe` websocket subscriptions.
///
/// `signatureSubscribe` is a one-shot subscription, meaning it is fulfilled by a single
/// notification and then discarded. Due to the high potential volume of these subscriptions
/// (e.g., at 20,000 TPS, over a million can be created per minute), unresolved
/// subscriptions could rapidly accumulate, leading to memory exhaustion.
///
/// This expirer implements a time-to-live (TTL) mechanism to mitigate this risk.
/// Each subscription is automatically removed after a 90-second duration if it has not
/// been fulfilled. This prevents resource leaks and protects the validator against
/// clients that may create subscriptions for nonexistent signatures.
///
/// An instance of `SignaturesExpirer` is created for each websocket connection.
pub(crate) struct SignaturesExpirer {
    /// A FIFO queue of subscriptions, ordered by creation time.
    ///
    /// This structure allows for efficient identification and
    /// removal of the oldest, already expired, subscriptions.
    pub(crate) cache: VecDeque<ExpiringSignature>,

    /// A monotonically increasing counter used as a lightweight timestamp.
    ///
    /// This value marks the creation "tick" of a subscription, avoiding the
    /// overhead of more complex time-tracking types for TTL calculations.
    tick: u64,

    /// An interval timer that triggers periodic checks for expired subscriptions.
    ticker: Interval,
}

/// A wrapper for a `Signature` that includes metadata for expiration tracking.
pub(crate) struct ExpiringSignature {
    /// The value of the expirer's `tick` at which this signature should expire.
    ttl: u64,
    /// The transaction signature being tracked.
    pub(crate) signature: Signature,
    /// A shared flag indicating if the subscription is still active. If the subscription
    /// resolves by itself this will be set to `false`, allowing the expirer to discard
    /// the signature without touching the subscriptions database (which is more expensive)
    subscribed: Arc<AtomicBool>,
}

impl SignaturesExpirer {
    /// The interval in seconds at which the expirer checks for expired subscriptions.
    const WAIT: u64 = 5;
    /// The Time-To-Live for a signature, expressed in the number of ticks.
    /// With a 90-second lifetime and a 5-second tick interval, a signature
    /// will expire after (90 / 5) = 18 ticks.
    const TTL: u64 = 90 / Self::WAIT;

    /// Initializes a new `SignaturesExpirer`.
    pub(crate) fn init() -> Self {
        Self {
            cache: Default::default(),
            tick: 0,
            ticker: time::interval(Duration::from_secs(Self::WAIT)),
        }
    }

    /// Adds a new signature to the expiration queue.
    ///
    /// The signature's expiration time is calculated by
    /// adding the `TTL` to the current `tick`.
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

    /// Asynchronously waits for and removes expired signatures from the queue.
    ///
    /// This method runs in a loop, advancing its internal `tick` every `WAIT`
    /// seconds. On each tick, it checks the front of the queue for signatures
    /// whose `ttl` has been reached.
    ///
    /// If an expired signature is found and is still marked as `subscribed`,
    /// this method returns it so that it can be removed from subscriptions
    /// database. If the subscription was resolved, it's silently discarded.
    pub(crate) async fn expire(&mut self) -> Signature {
        loop {
            // This inner block allows checking the queue multiple times per tick,
            // which efficiently clears out a batch of already-expired signatures.
            'expire: {
                // Peek at the oldest signature without removing it.
                let Some(s) = self.cache.front() else {
                    // The cache is empty, so break to await the next tick.
                    break 'expire;
                };

                // If the oldest signature's TTL is still in the future, stop checking.
                if s.ttl > self.tick {
                    break 'expire;
                }

                // The signature has expired, so remove it from the queue.
                let Some(s) = self.cache.pop_front() else {
                    // Should be unreachable due to the `front()` check above,
                    break 'expire;
                };

                // Only return the sibscription that hasn't resolved yet
                if s.subscribed.load(Ordering::Relaxed) {
                    return s.signature;
                }
            }

            // Wait for the ticker to fire before the next expiration check.
            self.ticker.tick().await;
            self.tick += 1;
        }
    }
}
