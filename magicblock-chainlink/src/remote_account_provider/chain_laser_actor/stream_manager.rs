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
/// - When the current-new stream exceeds [StreamManagerConfig::max_subs_in_new] it is
///   promoted to the [Self::unoptimized_old_streams] vec and a fresh current-new stream is created.
/// - When [Self::unoptimized_old_streams] exceed [StreamManagerConfig::max_old_unoptimized],
///   optimization is triggered which rebuilds all streams from the
///   `subscriptions` set into [StreamManager::optimized_old_streams] chunked by
///   [StreamManagerConfig::max_subs_in_old_optimized].
///
/// Unsubscribe only removes from the [Self::subscriptions] HashSet — it
/// never touches streams. Updates for unsubscribed pubkeys are
/// ignored at the actor level.
/// Unsubscribed accounts are dropped as part of optimization.
#[allow(unused)]
pub struct StreamManager<S: StreamFactory> {
    /// Configures limits for stream management
    config: StreamManagerConfig,
    /// The factory used to create streams
    stream_factory: S,
    /// Active streams for program subscriptions
    program_subscriptions: Option<(HashSet<Pubkey>, LaserStream)>,
    /// The canonical set of currently active account subscriptions.
    /// These include subscriptions maintained across the different set of streams,
    /// [Self::current_new_stream], [Self::unoptimized_old_streams], and
    /// [Self::optimized_old_streams].
    subscriptions: HashSet<Pubkey>,
    /// Pubkeys that are part of the current-new stream's filter.
    current_new_subs: HashSet<Pubkey>,
    /// The current-new stream which holds the [Self::current_new_subs].
    /// (None until the first subscribe call).
    current_new_stream: Option<LaserStream>,
    /// Old streams that have not been optimized yet.
    unoptimized_old_streams: Vec<LaserStream>,
    /// Old streams created by optimization, each covering up to
    /// [StreamManagerConfig::max_subs_in_old_optimized] subscriptions.
    optimized_old_streams: Vec<LaserStream>,
}

#[allow(unused)]
impl<S: StreamFactory> StreamManager<S> {
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

    // ---------------------
    // Account subscription
    // ---------------------

    /// Subscribe to account updates for the given pubkeys.
    ///
    /// Each pubkey is added to [Self::subscriptions] and to the [Self::current_new_stream].
    /// If the [Self::current_new_stream] exceeds [StreamManagerConfig::max_subs_in_new] it
    /// is promoted and a fresh one is created. If [Self::unoptimized_old_streams] exceed
    /// [StreamManagerConfig::max_old_unoptimized], optimization is triggered.
    pub fn account_subscribe(
        &mut self,
        pubkeys: &[Pubkey],
        commitment: &CommitmentLevel,
    ) {
        // Filter out pubkeys already in subscriptions.
        let new_pks: Vec<Pubkey> = pubkeys
            .iter()
            .filter(|pk| !self.subscriptions.contains(pk))
            .copied()
            .collect();

        if new_pks.is_empty() {
            return;
        }

        for pk in &new_pks {
            self.subscriptions.insert(*pk);
            self.current_new_subs.insert(*pk);
        }

        // (Re)create the current-new stream with the full
        // current_new_subs filter.
        self.current_new_stream =
            Some(self.create_account_stream(
                &self.current_new_subs.iter().collect::<Vec<_>>(),
                commitment,
            ));

        // Promote if current-new exceeds threshold.
        if self.current_new_subs.len() > self.config.max_subs_in_new {
            let overflow_count = self.current_new_subs.len()
                - self.config.max_subs_in_new;
            // The overflow pubkeys are the tail of new_pks.
            let overflow_start = new_pks.len().saturating_sub(
                overflow_count,
            );
            let overflow_pks = &new_pks[overflow_start..];

            // Move current-new stream to unoptimized old.
            if let Some(stream) = self.current_new_stream.take() {
                self.unoptimized_old_streams.push(stream);
            }
            self.current_new_subs.clear();

            // Start fresh current-new with overflow pubkeys.
            if overflow_pks.is_empty() {
                self.current_new_stream = None;
            } else {
                for pk in overflow_pks {
                    self.current_new_subs.insert(*pk);
                }
                self.current_new_stream =
                    Some(self.create_account_stream(
                        &overflow_pks
                            .iter()
                            .collect::<Vec<_>>(),
                        commitment,
                    ));
            }

            // If unoptimized old streams exceed the limit, optimize.
            if self.unoptimized_old_streams.len()
                > self.config.max_old_unoptimized
            {
                self.optimize(commitment);
            }
        }
    }

    /// Unsubscribe the given pubkeys.
    ///
    /// Removes them from the `subscriptions` HashSet only — streams
    /// are never modified. Updates for these pubkeys will be ignored
    /// by the actor.
    pub fn account_unsubscribe(&mut self, pubkeys: &[Pubkey]) {
        for pk in pubkeys {
            self.subscriptions.remove(pk);
        }
    }

    /// Rebuild all account streams from `subscriptions`.
    ///
    /// 1. Chunk `subscriptions` into groups of
    ///    `max_subs_in_old_optimized`.
    /// 2. Create a new stream for each chunk → `optimized_old_streams`.
    /// 3. Clear `unoptimized_old_streams`.
    /// 4. Reset the current-new stream (empty filter).
    pub fn optimize(&mut self, commitment: &CommitmentLevel) {
        // Collect all active subscriptions and chunk them.
        let all_pks: Vec<Pubkey> =
            self.subscriptions.iter().copied().collect();

        // Build optimized old streams from chunks.
        self.optimized_old_streams = all_pks
            .chunks(self.config.max_subs_in_old_optimized)
            .map(|chunk| {
                let refs: Vec<&Pubkey> = chunk.iter().collect();
                self.stream_factory.subscribe(
                    Self::build_account_request(&refs, commitment),
                )
            })
            .collect();

        // Clear unoptimized old streams.
        self.unoptimized_old_streams.clear();

        // Reset the current-new stream.
        self.current_new_subs.clear();
        self.current_new_stream = None;
    }

    /// Returns `true` if the pubkey is in the active `subscriptions`
    /// set.
    pub fn is_subscribed(&self, pubkey: &Pubkey) -> bool {
        self.subscriptions.contains(pubkey)
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
    fn current_new_sub_count(&self) -> usize {
        self.current_new_subs.len()
    }

    /// Returns a reference to the current-new stream's pubkey set.
    fn current_new_subs(&self) -> &HashSet<Pubkey> {
        &self.current_new_subs
    }

    /// Returns the number of unoptimized old streams.
    fn unoptimized_old_stream_count(&self) -> usize {
        self.unoptimized_old_streams.len()
    }

    /// Returns the number of optimized old streams.
    fn optimized_old_stream_count(&self) -> usize {
        self.optimized_old_streams.len()
    }

    /// Returns references to all account streams (optimized old +
    /// unoptimized old + current-new) for inspection.
    fn all_account_streams(&self) -> Vec<&LaserStream> {
        let mut streams = Vec::new();
        for s in &self.optimized_old_streams {
            streams.push(s);
        }
        for s in &self.unoptimized_old_streams {
            streams.push(s);
        }
        if let Some(s) = &self.current_new_stream {
            streams.push(s);
        }
        streams
    }

    /// Returns the total number of account streams across all
    /// generations.
    fn account_stream_count(&self) -> usize {
        let current = if self.current_new_stream.is_some() {
            1
        } else {
            0
        };
        self.optimized_old_streams.len()
            + self.unoptimized_old_streams.len()
            + current
    }

    // ---------------------------------------------------------
    // Internal helpers
    // ---------------------------------------------------------

    /// Build a `SubscribeRequest` for the given account pubkeys.
    /// Includes a slot subscription for chain slot synchronisation.
    fn build_account_request(
        pubkeys: &[&Pubkey],
        commitment: &CommitmentLevel,
    ) -> SubscribeRequest {
        let mut accounts = HashMap::new();
        accounts.insert(
            "account_subs".to_string(),
            SubscribeRequestFilterAccounts {
                account: pubkeys
                    .iter()
                    .map(|pk| pk.to_string())
                    .collect(),
                ..Default::default()
            },
        );

        let mut slots = HashMap::new();
        slots.insert(
            "slot_updates".to_string(),
            SubscribeRequestFilterSlots {
                filter_by_commitment: Some(true),
                ..Default::default()
            },
        );

        SubscribeRequest {
            accounts,
            slots,
            commitment: Some((*commitment).into()),
            ..Default::default()
        }
    }

    /// Build a `SubscribeRequest` and call the factory for the given
    /// account pubkeys.
    fn create_account_stream(
        &self,
        pubkeys: &[&Pubkey],
        commitment: &CommitmentLevel,
    ) -> LaserStream {
        let request =
            Self::build_account_request(pubkeys, commitment);
        self.stream_factory.subscribe(request)
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

    // ---------------------------------------------------------
    // Additional helpers
    // ---------------------------------------------------------

    const COMMITMENT: CommitmentLevel = CommitmentLevel::Processed;

    /// Subscribe `n` pubkeys one-at-a-time, returning the created
    /// pubkeys.
    fn subscribe_n(
        mgr: &mut StreamManager<MockStreamFactory>,
        n: usize,
    ) -> Vec<Pubkey> {
        let pks = make_pubkeys(n);
        mgr.account_subscribe(&pks, &COMMITMENT);
        pks
    }

    /// Subscribe pubkeys in batches of `batch` until `total` pubkeys
    /// have been subscribed. Returns all created pubkeys.
    fn subscribe_in_batches(
        mgr: &mut StreamManager<MockStreamFactory>,
        total: usize,
        batch: usize,
    ) -> Vec<Pubkey> {
        let mut all = Vec::new();
        let mut remaining = total;
        while remaining > 0 {
            let n = remaining.min(batch);
            let pks = make_pubkeys(n);
            mgr.account_subscribe(&pks, &COMMITMENT);
            all.extend(pks);
            remaining -= n;
        }
        all
    }

    /// Returns the union of all account pubkey strings across all
    /// captured requests from `start_idx` onward.
    fn all_filter_pubkeys_from(
        factory: &MockStreamFactory,
        start_idx: usize,
    ) -> HashSet<String> {
        factory
            .captured_requests()
            .iter()
            .skip(start_idx)
            .flat_map(account_pubkeys_from_request)
            .collect()
    }

    // -------------------------------------------------------------
    // 1. Subscription Tracking
    // -------------------------------------------------------------

    #[test]
    fn test_subscribe_single_pubkey_adds_to_subscriptions() {
        let (mut mgr, factory) = create_manager();
        let pk = Pubkey::new_unique();

        mgr.account_subscribe(&[pk], &COMMITMENT);

        assert_subscriptions_eq(&mgr, &[pk]);

        let reqs = factory.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_request_has_exact_pubkeys(&reqs[0], &[pk]);
    }

    #[test]
    fn test_subscribe_multiple_pubkeys_at_once() {
        let (mut mgr, factory) = create_manager();
        let pks = make_pubkeys(5);

        mgr.account_subscribe(&pks, &COMMITMENT);

        assert_subscriptions_eq(&mgr, &pks);

        let reqs = factory.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_request_has_exact_pubkeys(&reqs[0], &pks);
    }

    #[test]
    fn test_subscribe_duplicate_pubkey_is_noop() {
        let (mut mgr, factory) = create_manager();
        let pk = Pubkey::new_unique();

        mgr.account_subscribe(&[pk], &COMMITMENT);
        let calls_after_first = factory.captured_requests().len();

        mgr.account_subscribe(&[pk], &COMMITMENT);

        assert_subscriptions_eq(&mgr, &[pk]);
        assert_eq!(factory.captured_requests().len(), calls_after_first);
    }

    #[test]
    fn test_subscribe_incremental_calls_accumulate() {
        let (mut mgr, factory) = create_manager();
        let pks = make_pubkeys(3);

        mgr.account_subscribe(&[pks[0]], &COMMITMENT);
        mgr.account_subscribe(&[pks[1]], &COMMITMENT);
        mgr.account_subscribe(&[pks[2]], &COMMITMENT);

        assert_subscriptions_eq(&mgr, &pks);

        let reqs = factory.captured_requests();
        let last_req = reqs.last().unwrap();
        assert_request_has_exact_pubkeys(last_req, &pks);
    }

    // -------------------------------------------------------------
    // 2. Current-New Stream Lifecycle
    // -------------------------------------------------------------

    #[test]
    fn test_new_stream_created_on_first_subscribe() {
        let (mut mgr, factory) = create_manager();
        assert_eq!(mgr.account_stream_count(), 0);

        subscribe_n(&mut mgr, 1);

        assert_eq!(mgr.account_stream_count(), 1);
        assert_eq!(factory.active_stream_count(), 1);
    }

    #[test]
    fn test_current_new_stream_stays_below_threshold() {
        let (mut mgr, _factory) = create_manager();
        // MAX_NEW - 1 = 4
        subscribe_in_batches(&mut mgr, 4, 2);

        assert_eq!(mgr.account_stream_count(), 1);
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
    }

    #[test]
    fn test_current_new_stream_promoted_at_threshold() {
        let (mut mgr, factory) = create_manager();
        // Subscribe MAX_NEW (5) pubkeys first.
        let first_five = make_pubkeys(5);
        mgr.account_subscribe(&first_five, &COMMITMENT);
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);

        // Subscribe the 6th pubkey → triggers promotion.
        let sixth = Pubkey::new_unique();
        mgr.account_subscribe(&[sixth], &COMMITMENT);

        assert_eq!(mgr.unoptimized_old_stream_count(), 1);
        // A new current-new stream was created for the 6th pubkey.
        assert!(mgr.current_new_subs().contains(&sixth));
        // The factory received a new subscribe call for the fresh
        // current-new stream.
        let reqs = factory.captured_requests();
        assert!(reqs.len() >= 2);
    }

    #[test]
    fn test_multiple_promotions_accumulate_unoptimized() {
        let (mut mgr, _factory) = create_manager();
        // First promotion: subscribe 6 pubkeys (exceeds MAX_NEW=5).
        subscribe_n(&mut mgr, 6);
        assert_eq!(mgr.unoptimized_old_stream_count(), 1);

        // Second promotion: subscribe 5 more to fill the new current,
        // then 1 more to exceed.
        subscribe_n(&mut mgr, 5);
        assert_eq!(mgr.unoptimized_old_stream_count(), 2);

        // Current-new stream should only hold the overflow pubkeys.
        assert!(mgr.current_new_sub_count() <= 1);
    }

    // -------------------------------------------------------------
    // 3. Optimization Trigger via MAX_OLD_UNOPTIMIZED
    // -------------------------------------------------------------

    #[test]
    fn test_optimization_triggered_when_unoptimized_exceeds_max() {
        let (mut mgr, _factory) = create_manager();
        // MAX_OLD_UNOPTIMIZED = 3. We need 4 promotions.
        // Each promotion needs > MAX_NEW (5) pubkeys in current-new.
        // Subscribe 6 four times → 4 promotions.
        for _ in 0..3 {
            subscribe_n(&mut mgr, 6);
        }
        assert_eq!(mgr.unoptimized_old_stream_count(), 3);

        // 4th promotion triggers optimization.
        subscribe_n(&mut mgr, 6);

        // After optimization: unoptimized should be empty.
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
        // Optimized old streams should exist.
        let total_subs = mgr.subscriptions().len();
        let expected_optimized =
            total_subs.div_ceil(10); // ceil(total / MAX_OLD_OPTIMIZED)
        assert_eq!(
            mgr.optimized_old_stream_count(),
            expected_optimized,
        );
    }

    #[test]
    fn test_optimization_not_triggered_below_max_unoptimized() {
        let (mut mgr, _factory) = create_manager();
        // Exactly MAX_OLD_UNOPTIMIZED (3) promotions.
        for _ in 0..3 {
            subscribe_n(&mut mgr, 6);
        }
        assert_eq!(mgr.unoptimized_old_stream_count(), 3);
        assert_eq!(mgr.optimized_old_stream_count(), 0);
    }

    // -------------------------------------------------------------
    // 4. Manual / Interval-Driven Optimization
    // -------------------------------------------------------------

    #[test]
    fn test_optimize_creates_correct_number_of_optimized_streams() {
        let (mut mgr, _factory) = create_manager();
        subscribe_n(&mut mgr, 25);

        mgr.optimize(&COMMITMENT);

        // ceil(25 / 10) = 3
        assert_eq!(mgr.optimized_old_stream_count(), 3);
    }

    #[test]
    fn test_optimize_clears_unoptimized_old_streams() {
        let (mut mgr, _factory) = create_manager();
        // Create several unoptimized old streams.
        for _ in 0..3 {
            subscribe_n(&mut mgr, 6);
        }
        assert!(mgr.unoptimized_old_stream_count() > 0);

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
        assert!(mgr.optimized_old_stream_count() > 0);
    }

    #[test]
    fn test_optimize_resets_current_new_stream() {
        let (mut mgr, _factory) = create_manager();
        subscribe_n(&mut mgr, 8);

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.current_new_sub_count(), 0);
    }

    #[test]
    fn test_optimize_excludes_unsubscribed_pubkeys() {
        let (mut mgr, factory) = create_manager();
        let pks = subscribe_n(&mut mgr, 15);

        // Unsubscribe 5 of them.
        let to_unsub: Vec<Pubkey> = pks[0..5].to_vec();
        mgr.account_unsubscribe(&to_unsub);

        let reqs_before = factory.captured_requests().len();
        mgr.optimize(&COMMITMENT);

        // Optimized streams should only contain the 10 remaining
        // pubkeys.
        let remaining: HashSet<String> =
            pks[5..].iter().map(|pk| pk.to_string()).collect();
        let filter_pks =
            all_filter_pubkeys_from(&factory, reqs_before);
        assert_eq!(filter_pks.len(), 10);
        for pk in &to_unsub {
            assert!(
                !filter_pks.contains(&pk.to_string()),
                "unsubscribed pubkey {pk} found in optimized filter",
            );
        }
        for pk_str in &remaining {
            assert!(
                filter_pks.contains(pk_str),
                "expected pubkey {pk_str} missing from optimized filter",
            );
        }
    }

    #[test]
    fn test_optimize_with_zero_subscriptions() {
        let (mut mgr, _factory) = create_manager();
        let pks = subscribe_n(&mut mgr, 5);
        mgr.account_unsubscribe(&pks);

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.optimized_old_stream_count(), 0);
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
    }

    #[test]
    fn test_optimize_idempotent() {
        let (mut mgr, _factory) = create_manager();
        subscribe_n(&mut mgr, 15);

        mgr.optimize(&COMMITMENT);
        let count_after_first = mgr.optimized_old_stream_count();

        mgr.optimize(&COMMITMENT);
        assert_eq!(
            mgr.optimized_old_stream_count(),
            count_after_first,
        );
    }

    // -------------------------------------------------------------
    // 5. Behavior During Optimization
    // -------------------------------------------------------------

    #[test]
    fn test_subscribe_during_optimization_goes_to_current_new() {
        let (mut mgr, _factory) = create_manager();
        subscribe_n(&mut mgr, 20);

        mgr.optimize(&COMMITMENT);

        // Subscribe a new pubkey after optimization.
        let new_pk = Pubkey::new_unique();
        mgr.account_subscribe(&[new_pk], &COMMITMENT);

        assert!(mgr.subscriptions().contains(&new_pk));
        assert!(mgr.current_new_subs().contains(&new_pk));
    }

    #[test]
    fn test_no_double_optimization_trigger() {
        let (mut mgr, _factory) = create_manager();
        // Fill up to MAX_OLD_UNOPTIMIZED.
        for _ in 0..3 {
            subscribe_n(&mut mgr, 6);
        }
        assert_eq!(mgr.unoptimized_old_stream_count(), 3);

        // 4th promotion triggers optimization.
        subscribe_n(&mut mgr, 6);
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
        let optimized_after_first = mgr.optimized_old_stream_count();

        // Now subscribe enough to exceed MAX_SUBS_IN_NEW again,
        // causing a promotion. Since optimization just ran, it should
        // NOT trigger again immediately.
        subscribe_n(&mut mgr, 6);
        // Unoptimized grows by 1 but no second optimization.
        assert!(mgr.unoptimized_old_stream_count() <= 1);
        assert_eq!(
            mgr.optimized_old_stream_count(),
            optimized_after_first,
        );
    }

    // -------------------------------------------------------------
    // 6. Unsubscribe
    // -------------------------------------------------------------

    #[test]
    fn test_unsubscribe_removes_from_subscriptions_set() {
        let (mut mgr, _factory) = create_manager();
        let pks = make_pubkeys(3);
        mgr.account_subscribe(&pks, &COMMITMENT);

        mgr.account_unsubscribe(&[pks[1]]);

        assert_subscriptions_eq(&mgr, &[pks[0], pks[2]]);
    }

    #[test]
    fn test_unsubscribe_nonexistent_pubkey_is_noop() {
        let (mut mgr, _factory) = create_manager();
        let random = Pubkey::new_unique();

        mgr.account_unsubscribe(&[random]);

        assert!(mgr.subscriptions().is_empty());
    }

    #[test]
    fn test_unsubscribe_already_unsubscribed_pubkey() {
        let (mut mgr, _factory) = create_manager();
        let pk = Pubkey::new_unique();
        mgr.account_subscribe(&[pk], &COMMITMENT);

        mgr.account_unsubscribe(&[pk]);
        mgr.account_unsubscribe(&[pk]);

        assert!(mgr.subscriptions().is_empty());
    }

    #[test]
    fn test_unsubscribe_does_not_modify_streams() {
        let (mut mgr, factory) = create_manager();
        let pks = make_pubkeys(4);
        mgr.account_subscribe(&pks, &COMMITMENT);
        let calls_before = factory.captured_requests().len();

        mgr.account_unsubscribe(&pks[0..2]);

        // No new factory calls after unsubscribe.
        assert_eq!(factory.captured_requests().len(), calls_before);
        // Current-new subs still contain all 4 (streams not updated).
        for pk in &pks {
            assert!(mgr.current_new_subs().contains(pk));
        }
    }

    #[test]
    fn test_unsubscribe_all_then_optimize_clears_streams() {
        let (mut mgr, _factory) = create_manager();
        // Subscribe 8 pubkeys (creates current-new + 1 unoptimized).
        let pks = subscribe_n(&mut mgr, 8);
        mgr.account_unsubscribe(&pks);

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.optimized_old_stream_count(), 0);
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
    }

    #[test]
    fn test_unsubscribe_batch() {
        let (mut mgr, factory) = create_manager();
        let pks = make_pubkeys(5);
        mgr.account_subscribe(&pks, &COMMITMENT);
        let calls_before = factory.captured_requests().len();

        mgr.account_unsubscribe(&[pks[0], pks[2], pks[4]]);

        assert_subscriptions_eq(&mgr, &[pks[1], pks[3]]);
        assert_eq!(factory.captured_requests().len(), calls_before);
    }

    // -------------------------------------------------------------
    // 7. Subscription Membership Check
    // -------------------------------------------------------------

    #[test]
    fn test_is_subscribed_returns_true_for_active() {
        let (mut mgr, _factory) = create_manager();
        let pk = Pubkey::new_unique();
        mgr.account_subscribe(&[pk], &COMMITMENT);

        assert!(mgr.is_subscribed(&pk));
    }

    #[test]
    fn test_is_subscribed_returns_false_after_unsubscribe() {
        let (mut mgr, _factory) = create_manager();
        let pk = Pubkey::new_unique();
        mgr.account_subscribe(&[pk], &COMMITMENT);
        mgr.account_unsubscribe(&[pk]);

        assert!(!mgr.is_subscribed(&pk));
    }

    #[test]
    fn test_is_subscribed_returns_false_for_never_subscribed() {
        let (mgr, _factory) = create_manager();
        let random = Pubkey::new_unique();

        assert!(!mgr.is_subscribed(&random));
    }

    // -------------------------------------------------------------
    // 8. Stream Enumeration / Polling Access
    // -------------------------------------------------------------

    #[test]
    fn test_all_account_streams_includes_all_generations() {
        let (mut mgr, _factory) = create_manager();
        // Create optimized old streams.
        subscribe_n(&mut mgr, 15);
        mgr.optimize(&COMMITMENT);

        // Create an unoptimized old stream via promotion.
        subscribe_n(&mut mgr, 6);

        // Current-new also exists from the overflow pubkey.
        let expected = mgr.account_stream_count();
        let streams = mgr.all_account_streams();
        assert_eq!(streams.len(), expected);
    }

    #[test]
    fn test_all_account_streams_empty_when_no_subscriptions() {
        let (mgr, _factory) = create_manager();

        let streams = mgr.all_account_streams();
        assert!(streams.is_empty());
    }

    #[test]
    fn test_all_account_streams_after_optimize_drops_old_unoptimized()
    {
        let (mut mgr, _factory) = create_manager();
        // Create unoptimized old streams.
        for _ in 0..2 {
            subscribe_n(&mut mgr, 6);
        }
        assert!(mgr.unoptimized_old_stream_count() > 0);

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
        let streams = mgr.all_account_streams();
        // Only optimized old streams remain (current-new is empty
        // after optimize).
        assert_eq!(streams.len(), mgr.optimized_old_stream_count());
    }

    // -------------------------------------------------------------
    // 9. Edge Cases and Stress
    // -------------------------------------------------------------

    #[test]
    fn test_subscribe_exactly_at_max_subs_in_new_no_promotion() {
        let (mut mgr, _factory) = create_manager();
        // Exactly MAX_NEW (5) pubkeys — should NOT promote.
        subscribe_n(&mut mgr, 5);

        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
        assert_eq!(mgr.account_stream_count(), 1);
    }

    #[test]
    fn test_single_pubkey_optimization() {
        let (mut mgr, _factory) = create_manager();
        subscribe_n(&mut mgr, 1);

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.optimized_old_stream_count(), 1);
        assert_eq!(mgr.current_new_sub_count(), 0);
    }

    #[test]
    fn test_subscribe_max_old_optimized_plus_one() {
        let (mut mgr, _factory) = create_manager();
        // MAX_OLD_OPTIMIZED + 1 = 11
        subscribe_n(&mut mgr, 11);

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.optimized_old_stream_count(), 2);
    }

    #[test]
    fn test_large_scale_subscribe_and_optimize() {
        let (mut mgr, factory) = create_manager();
        let pks = subscribe_n(&mut mgr, 50);

        let reqs_before = factory.captured_requests().len();
        mgr.optimize(&COMMITMENT);

        // ceil(50 / 10) = 5
        assert_eq!(mgr.optimized_old_stream_count(), 5);
        assert_eq!(mgr.subscriptions().len(), 50);
        assert_eq!(mgr.current_new_sub_count(), 0);

        // Verify the union of all optimized stream filters equals all
        // 50 pubkeys.
        let filter_pks =
            all_filter_pubkeys_from(&factory, reqs_before);
        assert_eq!(filter_pks.len(), 50);
        for pk in &pks {
            assert!(filter_pks.contains(&pk.to_string()));
        }
    }

    #[test]
    fn test_interleaved_subscribe_unsubscribe_then_optimize() {
        let (mut mgr, factory) = create_manager();
        let pks = subscribe_n(&mut mgr, 20);
        // Unsubscribe 8 scattered.
        let unsub1: Vec<Pubkey> =
            pks.iter().step_by(2).take(8).copied().collect();
        mgr.account_unsubscribe(&unsub1);

        // Subscribe 5 new ones.
        let new_pks = subscribe_n(&mut mgr, 5);
        // Unsubscribe 2 of the new ones.
        mgr.account_unsubscribe(&new_pks[0..2]);

        let expected_count = 20 - 8 + 5 - 2;
        assert_eq!(mgr.subscriptions().len(), expected_count);

        let reqs_before = factory.captured_requests().len();
        mgr.optimize(&COMMITMENT);

        let filter_pks =
            all_filter_pubkeys_from(&factory, reqs_before);
        assert_eq!(filter_pks.len(), expected_count);
        // Verify unsubscribed pubkeys are absent.
        for pk in &unsub1 {
            assert!(!filter_pks.contains(&pk.to_string()));
        }
        for pk in &new_pks[0..2] {
            assert!(!filter_pks.contains(&pk.to_string()));
        }
    }

    #[test]
    fn test_rapid_subscribe_unsubscribe_same_pubkey() {
        let (mut mgr, _factory) = create_manager();
        let pk = Pubkey::new_unique();

        mgr.account_subscribe(&[pk], &COMMITMENT);
        mgr.account_unsubscribe(&[pk]);
        mgr.account_subscribe(&[pk], &COMMITMENT);

        assert!(mgr.subscriptions().contains(&pk));
        assert!(mgr.current_new_subs().contains(&pk));
    }

    // -------------------------------------------------------------
    // 10. Stream Factory Interaction Verification
    // -------------------------------------------------------------

    #[test]
    fn test_factory_called_with_correct_commitment() {
        let (mut mgr, factory) = create_manager();
        let commitment = CommitmentLevel::Finalized;
        let pk = Pubkey::new_unique();

        mgr.account_subscribe(&[pk], &commitment);

        let reqs = factory.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].commitment,
            Some(i32::from(CommitmentLevel::Finalized)),
        );
    }

    #[test]
    fn test_factory_called_with_slot_filter() {
        let (mut mgr, factory) = create_manager();
        subscribe_n(&mut mgr, 1);

        let reqs = factory.captured_requests();
        assert!(!reqs[0].slots.is_empty());
    }

    #[test]
    fn test_optimize_factory_calls_contain_chunked_pubkeys() {
        let (mut mgr, factory) = create_manager();
        subscribe_n(&mut mgr, 15);

        let reqs_before = factory.captured_requests().len();
        mgr.optimize(&COMMITMENT);

        let optimize_reqs: Vec<_> = factory
            .captured_requests()
            .into_iter()
            .skip(reqs_before)
            .collect();
        assert_eq!(optimize_reqs.len(), 2);

        let first_pks = account_pubkeys_from_request(&optimize_reqs[0]);
        let second_pks =
            account_pubkeys_from_request(&optimize_reqs[1]);
        assert_eq!(first_pks.len(), 10);
        assert_eq!(second_pks.len(), 5);

        // No overlap.
        assert!(first_pks.is_disjoint(&second_pks));
    }

    #[test]
    fn test_factory_not_called_on_unsubscribe() {
        let (mut mgr, factory) = create_manager();
        subscribe_n(&mut mgr, 5);
        let calls_before = factory.captured_requests().len();

        let pks: Vec<Pubkey> =
            mgr.subscriptions().iter().take(3).copied().collect();
        mgr.account_unsubscribe(&pks);

        assert_eq!(factory.captured_requests().len(), calls_before);
    }
}
