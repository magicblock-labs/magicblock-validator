use std::collections::{HashMap, HashSet};

use helius_laserstream::grpc::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterSlots,
};
use solana_pubkey::Pubkey;

use super::{LaserStream, StreamFactory};

/// Configuration for the generational stream manager.
#[allow(unused)]
pub struct StreamManagerConfig {
    /// Max subscriptions per optimized old stream chunk.
    pub max_subs_in_old_optimized: usize,
    /// Max unoptimized old streams before optimization is triggered.
    pub max_old_unoptimized: usize,
    /// Max subscriptions in the current-new stream before it is
    /// promoted to an unoptimized old stream.
    pub max_subs_in_new: usize,
}

impl Default for StreamManagerConfig {
    fn default() -> Self {
        Self {
            max_subs_in_old_optimized: 2000,
            max_old_unoptimized: 10,
            max_subs_in_new: 200,
        }
    }
}

/// Manages the creation and lifecycle of GRPC laser streams.
///
/// Account subscriptions follow a generational approach:
/// - New subscriptions go into the *current-new* stream.
/// - When the current-new stream exceeds `max_subs_in_new` it is
///   promoted to the *unoptimized old* streams vec and a fresh
///   current-new stream is created.
/// - When unoptimized old streams exceed `max_old_unoptimized`,
///   optimization is triggered which rebuilds all streams from the
///   `subscriptions` set into *optimized old* streams chunked by
///   `max_subs_in_old_optimized`.
///
/// Unsubscribe only removes from the `subscriptions` HashSet — it
/// never touches streams. Updates for unsubscribed pubkeys are
/// ignored at the actor level.
#[allow(unused)]
pub struct StreamManager<S: StreamFactory> {
    config: StreamManagerConfig,
    stream_factory: S,

    // ----- Program subscriptions (unchanged) -----
    /// Active streams for program subscriptions
    program_subscriptions: Option<(HashSet<Pubkey>, LaserStream)>,

    // ----- Generational account subscriptions -----
    /// The canonical set of currently active account subscriptions.
    subscriptions: HashSet<Pubkey>,
    /// Pubkeys that are part of the current-new stream's filter.
    current_new_subs: HashSet<Pubkey>,
    /// The current-new stream (None until the first subscribe call).
    current_new_stream: Option<LaserStream>,
    /// Old streams that have not been optimized yet.
    unoptimized_old_streams: Vec<LaserStream>,
    /// Old streams created by optimization, each covering up to
    /// `max_subs_in_old_optimized` subscriptions.
    optimized_old_streams: Vec<LaserStream>,
}

#[allow(unused)]
impl<S: StreamFactory> StreamManager<S> {
    /// Creates a new stream manager with the given config and stream
    /// factory.
    pub fn new(config: StreamManagerConfig, stream_factory: S) -> Self {
        Self {
            config,
            stream_factory,
            program_subscriptions: None,
            subscriptions: HashSet::new(),
            current_new_subs: HashSet::new(),
            current_new_stream: None,
            unoptimized_old_streams: Vec::new(),
            optimized_old_streams: Vec::new(),
        }
    }

    // ---------------------------------------------------------
    // Account subscription — generational API (stubs)
    // ---------------------------------------------------------

    /// Subscribe to account updates for the given pubkeys.
    ///
    /// Each pubkey is added to `subscriptions` and to the current-new
    /// stream. If the current-new stream exceeds `max_subs_in_new` it
    /// is promoted and a fresh one is created. If unoptimized old
    /// streams exceed `max_old_unoptimized`, optimization is
    /// triggered.
    pub fn account_subscribe(
        &mut self,
        _pubkeys: &[Pubkey],
        _commitment: &CommitmentLevel,
    ) {
        todo!("account_subscribe: generational implementation")
    }

    /// Unsubscribe the given pubkeys.
    ///
    /// Removes them from the `subscriptions` HashSet only — streams
    /// are never modified. Updates for these pubkeys will be ignored
    /// by the actor.
    pub fn account_unsubscribe(&mut self, _pubkeys: &[Pubkey]) {
        todo!("account_unsubscribe")
    }

    /// Rebuild all account streams from `subscriptions`.
    ///
    /// 1. Chunk `subscriptions` into groups of
    ///    `max_subs_in_old_optimized`.
    /// 2. Create a new stream for each chunk → `optimized_old_streams`.
    /// 3. Clear `unoptimized_old_streams`.
    /// 4. Reset the current-new stream (empty filter).
    pub fn optimize(&mut self, _commitment: &CommitmentLevel) {
        todo!("optimize")
    }

    /// Returns `true` if the pubkey is in the active `subscriptions`
    /// set.
    pub fn is_subscribed(&self, _pubkey: &Pubkey) -> bool {
        todo!("is_subscribed")
    }

    // ---------------------------------------------------------
    // Accessors — internal state inspection
    // ---------------------------------------------------------

    /// Returns a reference to the canonical subscriptions set.
    pub fn subscriptions(&self) -> &HashSet<Pubkey> {
        &self.subscriptions
    }

    /// Returns the number of pubkeys in the current-new stream's
    /// filter.
    pub fn current_new_sub_count(&self) -> usize {
        self.current_new_subs.len()
    }

    /// Returns a reference to the current-new stream's pubkey set.
    pub fn current_new_subs(&self) -> &HashSet<Pubkey> {
        &self.current_new_subs
    }

    /// Returns the number of unoptimized old streams.
    pub fn unoptimized_old_stream_count(&self) -> usize {
        self.unoptimized_old_streams.len()
    }

    /// Returns the number of optimized old streams.
    pub fn optimized_old_stream_count(&self) -> usize {
        self.optimized_old_streams.len()
    }

    /// Returns mutable references to all account streams (optimized
    /// old + unoptimized old + current-new) for polling.
    pub fn all_account_streams_mut(&mut self) -> Vec<&mut LaserStream> {
        todo!("all_account_streams_mut")
    }

    /// Returns the total number of account streams across all
    /// generations.
    pub fn account_stream_count(&self) -> usize {
        let current = if self.current_new_stream.is_some() {
            1
        } else {
            0
        };
        self.optimized_old_streams.len()
            + self.unoptimized_old_streams.len()
            + current
    }

    // =========================================================
    // Legacy account subscribe (kept for migration)
    // =========================================================

    /// Creates a subscription stream for account updates (legacy).
    ///
    /// It includes a slot subscription for chain slot synchronization.
    pub fn account_subscribe_old(
        &self,
        pubkeys: &[&Pubkey],
        commitment: &CommitmentLevel,
        idx: usize,
        from_slot: Option<u64>,
    ) -> super::LaserStream {
        let mut accounts = HashMap::new();
        accounts.insert(
            format!("account_subs: {idx}"),
            SubscribeRequestFilterAccounts {
                account: pubkeys.iter().map(|pk| pk.to_string()).collect(),
                ..Default::default()
            },
        );

        // Subscribe to slot updates for chain_slot synchronization
        let mut slots = HashMap::new();
        slots.insert(
            "slot_updates".to_string(),
            SubscribeRequestFilterSlots {
                filter_by_commitment: Some(true),
                ..Default::default()
            },
        );

        let request = SubscribeRequest {
            accounts,
            slots,
            commitment: Some((*commitment).into()),
            from_slot,
            ..Default::default()
        };
        self.stream_factory.subscribe(request)
    }

    /// Adds a program subscription. If the program is already
    /// subscribed, this is a no-op. Otherwise, recreates the program
    /// stream to include all subscribed programs.
    pub fn add_program_subscription(
        &mut self,
        program_id: Pubkey,
        commitment: &CommitmentLevel,
    ) {
        if self
            .program_subscriptions
            .as_ref()
            .is_some_and(|(subs, _)| subs.contains(&program_id))
        {
            return;
        }

        let mut subscribed_programs = self
            .program_subscriptions
            .as_ref()
            .map(|(subs, _)| subs.clone())
            .unwrap_or_default();

        subscribed_programs.insert(program_id);

        let program_ids: Vec<&Pubkey> = subscribed_programs.iter().collect();
        let stream = self.create_program_stream(&program_ids, commitment);
        self.program_subscriptions = Some((subscribed_programs, stream));
    }

    /// Returns a mutable reference to the program subscriptions
    /// stream (if any) for polling in the actor loop.
    pub fn program_stream_mut(&mut self) -> Option<&mut LaserStream> {
        self.program_subscriptions.as_mut().map(|(_, s)| s)
    }

    /// Returns whether there are active program subscriptions.
    pub fn has_program_subscriptions(&self) -> bool {
        self.program_subscriptions.is_some()
    }

    /// Clears all program subscriptions.
    pub fn clear_program_subscriptions(&mut self) {
        self.program_subscriptions = None;
    }

    /// Creates a subscription stream for program updates.
    fn create_program_stream(
        &self,
        program_ids: &[&Pubkey],
        commitment: &CommitmentLevel,
    ) -> LaserStream {
        let mut accounts = HashMap::new();
        accounts.insert(
            "program_sub".to_string(),
            SubscribeRequestFilterAccounts {
                owner: program_ids.iter().map(|pk| pk.to_string()).collect(),
                ..Default::default()
            },
        );
        let request = SubscribeRequest {
            accounts,
            commitment: Some((*commitment).into()),
            ..Default::default()
        };
        self.stream_factory.subscribe(request)
    }
}

#[cfg(test)]
mod tests {
    use helius_laserstream::grpc::CommitmentLevel;
    use solana_pubkey::Pubkey;

    use super::*;
    use crate::remote_account_provider::chain_laser_actor::mock::MockStreamFactory;

    // -----------------
    // Helpers
    // -----------------
    fn test_config() -> StreamManagerConfig {
        StreamManagerConfig {
            max_subs_in_old_optimized: 10,
            max_old_unoptimized: 3,
            max_subs_in_new: 5,
        }
    }

    fn create_manager() -> (StreamManager<MockStreamFactory>, MockStreamFactory) {
        let factory = MockStreamFactory::new();
        let manager = StreamManager::new(test_config(), factory.clone());
        (manager, factory)
    }

    fn make_pubkeys(n: usize) -> Vec<Pubkey> {
        (0..n).map(|_| Pubkey::new_unique()).collect()
    }

    /// Collect all account pubkey strings from a captured
    /// `SubscribeRequest`'s account filters.
    fn account_pubkeys_from_request(req: &SubscribeRequest) -> HashSet<String> {
        req.accounts
            .values()
            .flat_map(|f| f.account.iter().cloned())
            .collect()
    }

    /// Assert that `subscriptions()` contains exactly `expected`
    /// (order-independent, exact count).
    fn assert_subscriptions_eq(
        mgr: &StreamManager<MockStreamFactory>,
        expected: &[Pubkey],
    ) {
        let subs = mgr.subscriptions();
        assert_eq!(
            subs.len(),
            expected.len(),
            "expected {} subscriptions, got {}",
            expected.len(),
            subs.len(),
        );
        for pk in expected {
            assert!(subs.contains(pk), "subscription set missing pubkey {pk}",);
        }
    }

    /// Assert that a `SubscribeRequest` filter contains exactly the
    /// given pubkeys (order-independent, exact count).
    fn assert_request_has_exact_pubkeys(
        req: &SubscribeRequest,
        expected: &[Pubkey],
    ) {
        let filter = account_pubkeys_from_request(req);
        assert_eq!(
            filter.len(),
            expected.len(),
            "expected {} pubkeys in filter, got {}",
            expected.len(),
            filter.len(),
        );
        for pk in expected {
            assert!(
                filter.contains(&pk.to_string()),
                "request filter missing pubkey {pk}",
            );
        }
    }

    // -------------------------------------------------------------
    // 1. Subscription Tracking
    // -------------------------------------------------------------

    #[test]
    fn test_subscribe_single_pubkey_adds_to_subscriptions() {
        let (mut mgr, factory) = create_manager();
        let pk = Pubkey::new_unique();
        let commitment = CommitmentLevel::Processed;

        mgr.account_subscribe(&[pk], &commitment);

        assert_subscriptions_eq(&mgr, &[pk]);

        // Exactly one subscribe call (initial current-new stream)
        // whose filter contains exactly this pubkey.
        let reqs = factory.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_request_has_exact_pubkeys(&reqs[0], &[pk]);
    }

    #[test]
    fn test_subscribe_multiple_pubkeys_at_once() {
        let (mut mgr, factory) = create_manager();
        let pks = make_pubkeys(5);
        let commitment = CommitmentLevel::Processed;

        mgr.account_subscribe(&pks, &commitment);

        assert_subscriptions_eq(&mgr, &pks);

        // One stream created whose filter contains all 5.
        let reqs = factory.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_request_has_exact_pubkeys(&reqs[0], &pks);
    }

    #[test]
    fn test_subscribe_duplicate_pubkey_is_noop() {
        let (mut mgr, factory) = create_manager();
        let pk = Pubkey::new_unique();
        let commitment = CommitmentLevel::Processed;

        mgr.account_subscribe(&[pk], &commitment);
        let calls_after_first = factory.captured_requests().len();

        // Subscribe the same pubkey again.
        mgr.account_subscribe(&[pk], &commitment);

        assert_subscriptions_eq(&mgr, &[pk]);

        // No extra streams spawned.
        assert_eq!(factory.captured_requests().len(), calls_after_first,);
    }

    #[test]
    fn test_subscribe_incremental_calls_accumulate() {
        let (mut mgr, factory) = create_manager();
        let pks = make_pubkeys(3);
        let commitment = CommitmentLevel::Processed;

        mgr.account_subscribe(&[pks[0]], &commitment);
        mgr.account_subscribe(&[pks[1]], &commitment);
        mgr.account_subscribe(&[pks[2]], &commitment);

        assert_subscriptions_eq(&mgr, &pks);

        // The most recent factory call's filter should contain all
        // three pubkeys.
        let reqs = factory.captured_requests();
        let last_req = reqs.last().unwrap();
        assert_request_has_exact_pubkeys(last_req, &pks);
    }
}
