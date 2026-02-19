use std::collections::{HashMap, HashSet};

use helius_laserstream::grpc::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterSlots,
};
use solana_pubkey::Pubkey;
use tokio::time::Duration;
use tokio_stream::StreamMap;

use super::{
    LaserResult, LaserStream, LaserStreamWithHandle, SharedSubscriptions,
    StreamFactory,
};
use crate::remote_account_provider::{
    chain_laser_actor::StreamHandle, RemoteAccountProviderError,
    RemoteAccountProviderResult,
};

/// Identifies whether a stream update came from an account or
/// program subscription stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamUpdateSource {
    Account,
    Program,
}

/// Identifies a stream within the [StreamMap].
///
/// Each variant maps to a stream category. The `usize` index
/// corresponds to the position within the respective `Vec` of
/// handles.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum StreamKey {
    CurrentNew,
    UnoptimizedOld(usize),
    OptimizedOld(usize),
    Program,
}

impl StreamKey {
    fn source(&self) -> StreamUpdateSource {
        match self {
            StreamKey::Program => StreamUpdateSource::Program,
            _ => StreamUpdateSource::Account,
        }
    }
}

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
///   promoted to the [Self::unoptimized_old_handles] vec and a fresh current-new stream is created.
/// - When [Self::unoptimized_old_handles] exceed [StreamManagerConfig::max_old_unoptimized],
///   optimization is triggered which rebuilds all streams from the
///   `subscriptions` set into [StreamManager::optimized_old_handles] chunked by
///   [StreamManagerConfig::max_subs_in_old_optimized].
///
/// Unsubscribe only removes from the [Self::subscriptions] HashSet — it
/// never touches streams. Updates for unsubscribed pubkeys are
/// ignored at the actor level.
/// Unsubscribed accounts are dropped as part of optimization.
///
/// Streams are stored in a persistent [StreamMap] keyed by
/// [StreamKey]. The map is only updated when stream topology
/// changes (subscribe, promote, optimize, clear). The
/// corresponding handles are stored separately for use in
/// [Self::update_subscriptions].
#[allow(unused)]
pub struct StreamManager<S: StreamHandle, SF: StreamFactory<S>> {
    /// Configures limits for stream management
    config: StreamManagerConfig,
    /// The factory used to create streams
    stream_factory: SF,
    /// The canonical set of currently active account subscriptions.
    /// These include subscriptions maintained across the different set
    /// of streams.
    subscriptions: SharedSubscriptions,
    /// Pubkeys that are part of the current-new stream's filter.
    current_new_subs: HashSet<Pubkey>,

    // -- Handles (needed for update_subscriptions) --
    /// Handle for the current-new stream.
    current_new_handle: Option<S>,
    /// Handles for unoptimized old streams.
    unoptimized_old_handles: Vec<S>,
    /// Handles for optimized old streams.
    optimized_old_handles: Vec<S>,
    /// Handle + pubkey set for program subscriptions.
    program_sub: Option<(HashSet<Pubkey>, S)>,

    // -- All streams live here. --
    /// Streams separated from the handles in order to allow using them
    /// inside a StreamMap
    /// They are addressed via the StreamKey which includes an index for
    /// [Self::unoptimized_old_handles] and [Self::optimized_old_handles].
    /// The key index matches the index of the corresponding vec.
    /// Persistent stream map polled by [Self::next_update].
    /// Updated only when stream topology changes.
    stream_map: StreamMap<StreamKey, LaserStream>,
}

#[allow(unused)]
impl<S: StreamHandle, SF: StreamFactory<S>> StreamManager<S, SF> {
    pub fn new(config: StreamManagerConfig, stream_factory: SF) -> Self {
        Self {
            config,
            stream_factory,
            subscriptions: Default::default(),
            current_new_subs: HashSet::new(),
            current_new_handle: None,
            unoptimized_old_handles: Vec::new(),
            optimized_old_handles: Vec::new(),
            program_sub: None,
            stream_map: StreamMap::new(),
        }
    }

    /// Update a stream's subscriptions with retry logic.
    ///
    /// Attempts to write the given request to the stream handle up to 5
    /// times with linear backoff. Returns an error if all retries are
    /// exhausted.
    async fn update_subscriptions(
        handle: &S,
        task: &str,
        request: SubscribeRequest,
    ) -> RemoteAccountProviderResult<()> {
        const MAX_RETRIES: usize = 5;
        let mut retries = MAX_RETRIES;
        let initial_retries = retries;

        loop {
            match handle.write(request.clone()).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    if retries > 0 {
                        retries -= 1;
                        // Linear backoff: sleep longer as retries decrease
                        let backoff_ms =
                            50u64 * (initial_retries - retries) as u64;
                        tokio::time::sleep(Duration::from_millis(backoff_ms))
                            .await;
                        continue;
                    }
                    return Err(RemoteAccountProviderError::GrpcSubscriptionUpdateFailed(
                        task.to_string(),
                        MAX_RETRIES,
                        format!("{err} ({err:?}"),
                    ));
                }
            }
        }
    }

    // ---------------------
    // Account subscription
    // ---------------------

    /// Subscribe to account updates for the given pubkeys.
    ///
    /// Each pubkey is added to [Self::subscriptions] and to the
    /// current-new stream. If the current-new stream exceeds
    /// [StreamManagerConfig::max_subs_in_new] it is promoted and
    /// a fresh one is created. If unoptimized old handles exceed
    /// [StreamManagerConfig::max_old_unoptimized], optimization
    /// is triggered.
    pub async fn account_subscribe(
        &mut self,
        pubkeys: &[Pubkey],
        commitment: &CommitmentLevel,
        from_slot: Option<u64>,
    ) -> RemoteAccountProviderResult<()> {
        // Filter out pubkeys already in subscriptions.
        let new_pks: Vec<Pubkey> = {
            let subs = self.subscriptions.read();
            pubkeys
                .iter()
                .filter(|pk| !subs.contains(pk))
                .copied()
                .collect()
        };

        if new_pks.is_empty() {
            return Ok(());
        }

        {
            let mut subs = self.subscriptions.write();
            for pk in &new_pks {
                subs.insert(*pk);
                self.current_new_subs.insert(*pk);
            }
        }

        // Update the current-new stream with the full
        // current_new_subs filter (either create new if doesn't
        // exist, or update existing via write).
        if let Some(handle) = &self.current_new_handle {
            let request = Self::build_account_request(
                &self.current_new_subs.iter().collect::<Vec<_>>(),
                commitment,
                from_slot,
            );
            Self::update_subscriptions(handle, "account_subscribe", request)
                .await?
        } else {
            let pks: Vec<Pubkey> =
                self.current_new_subs.iter().copied().collect();
            let pk_refs: Vec<&Pubkey> = pks.iter().collect();
            self.insert_current_new_stream(&pk_refs, commitment, from_slot);
        }

        // Promote if current-new exceeds threshold.
        if self.current_new_subs.len() > self.config.max_subs_in_new {
            let overflow_count =
                self.current_new_subs.len() - self.config.max_subs_in_new;
            // The overflow pubkeys are the tail of new_pks.
            let overflow_start = new_pks.len().saturating_sub(overflow_count);
            let overflow_pks = &new_pks[overflow_start..];

            // Move current-new to unoptimized old.
            if let Some(stream) = self.stream_map.remove(&StreamKey::CurrentNew)
            {
                let idx = self.unoptimized_old_handles.len();
                self.stream_map
                    .insert(StreamKey::UnoptimizedOld(idx), stream);
            }
            if let Some(handle) = self.current_new_handle.take() {
                self.unoptimized_old_handles.push(handle);
            }
            self.current_new_subs.clear();

            // Start fresh current-new with overflow pubkeys.
            if !overflow_pks.is_empty() {
                for pk in overflow_pks {
                    self.current_new_subs.insert(*pk);
                }
                self.insert_current_new_stream(
                    &overflow_pks.iter().collect::<Vec<_>>(),
                    commitment,
                    from_slot,
                );
            }

            // If unoptimized old handles exceed the limit,
            // optimize.
            if self.unoptimized_old_handles.len()
                > self.config.max_old_unoptimized
            {
                self.optimize(commitment);
            }
        }

        Ok(())
    }

    /// Unsubscribe the given pubkeys.
    ///
    /// Removes them from the `subscriptions` HashSet only — streams
    /// are never modified. Updates for these pubkeys will be ignored
    /// by the actor.
    pub fn account_unsubscribe(&mut self, pubkeys: &[Pubkey]) {
        let mut subs = self.subscriptions.write();
        for pk in pubkeys {
            subs.remove(pk);
        }
    }

    /// Clears all account subscriptions and drops all account
    /// streams.
    pub fn clear_account_subscriptions(&mut self) {
        self.subscriptions.write().clear();
        self.current_new_subs.clear();
        self.current_new_handle = None;
        self.stream_map.remove(&StreamKey::CurrentNew);
        for i in 0..self.unoptimized_old_handles.len() {
            self.stream_map.remove(&StreamKey::UnoptimizedOld(i));
        }
        self.unoptimized_old_handles.clear();
        for i in 0..self.optimized_old_handles.len() {
            self.stream_map.remove(&StreamKey::OptimizedOld(i));
        }
        self.optimized_old_handles.clear();
    }

    /// Returns `true` if any account stream exists.
    pub fn has_account_subscriptions(&self) -> bool {
        self.current_new_handle.is_some()
            || !self.unoptimized_old_handles.is_empty()
            || !self.optimized_old_handles.is_empty()
    }

    /// Polls all streams in the [StreamMap], returning the next
    /// available update tagged with its source.
    /// Returns `None` when the map is empty.
    pub async fn next_update(
        &mut self,
    ) -> Option<(StreamUpdateSource, LaserResult)> {
        use tokio_stream::StreamExt;
        let (key, result) = self.stream_map.next().await?;
        Some((key.source(), result))
    }

    /// Returns `true` if any stream (account or program) exists.
    pub fn has_any_subscriptions(&self) -> bool {
        !self.stream_map.is_empty()
    }

    /// Rebuild all account streams from `subscriptions`.
    ///
    /// 1. Chunk `subscriptions` into groups of
    ///    `max_subs_in_old_optimized`.
    /// 2. Create a new stream for each chunk →
    ///    `optimized_old_handles`.
    /// 3. Clear `unoptimized_old_handles`.
    /// 4. Reset the current-new stream (empty filter).
    pub fn optimize(&mut self, commitment: &CommitmentLevel) {
        // Remove all account streams from the map.
        self.stream_map.remove(&StreamKey::CurrentNew);
        for i in 0..self.unoptimized_old_handles.len() {
            self.stream_map.remove(&StreamKey::UnoptimizedOld(i));
        }
        for i in 0..self.optimized_old_handles.len() {
            self.stream_map.remove(&StreamKey::OptimizedOld(i));
        }

        // Collect all active subscriptions and chunk them.
        let all_pks: Vec<Pubkey> =
            self.subscriptions.read().iter().copied().collect();

        // Build optimized old streams from chunks.
        self.optimized_old_handles = Vec::new();
        for (i, chunk) in all_pks
            .chunks(self.config.max_subs_in_old_optimized)
            .enumerate()
        {
            let refs: Vec<&Pubkey> = chunk.iter().collect();
            let LaserStreamWithHandle { stream, handle } =
                self.stream_factory.subscribe(Self::build_account_request(
                    &refs, commitment, None,
                ));
            self.stream_map.insert(StreamKey::OptimizedOld(i), stream);
            self.optimized_old_handles.push(handle);
        }

        // Clear unoptimized old handles.
        self.unoptimized_old_handles.clear();

        // Reset the current-new stream.
        self.current_new_subs.clear();
        self.current_new_handle = None;
    }

    /// Returns `true` if the pubkey is in the active
    /// `subscriptions` set.
    pub fn is_subscribed(&self, pubkey: &Pubkey) -> bool {
        self.subscriptions.read().contains(pubkey)
    }

    // ---------------------------------------------------------
    // Accessors — internal state inspection
    // ---------------------------------------------------------

    /// Returns a reference to the shared subscriptions.
    pub fn subscriptions(&self) -> &SharedSubscriptions {
        &self.subscriptions
    }

    /// Returns the number of pubkeys in the current-new stream's
    /// filter.
    fn current_new_sub_count(&self) -> usize {
        self.current_new_subs.len()
    }

    /// Returns a reference to the current-new stream's pubkey
    /// set.
    fn current_new_subs(&self) -> &HashSet<Pubkey> {
        &self.current_new_subs
    }

    /// Returns the number of unoptimized old streams.
    fn unoptimized_old_stream_count(&self) -> usize {
        self.unoptimized_old_handles.len()
    }

    /// Returns the number of optimized old streams.
    fn optimized_old_stream_count(&self) -> usize {
        self.optimized_old_handles.len()
    }

    /// Returns the total number of account streams across all
    /// generations.
    fn account_stream_count(&self) -> usize {
        let current = usize::from(self.current_new_handle.is_some());
        self.optimized_old_handles.len()
            + self.unoptimized_old_handles.len()
            + current
    }

    // ---------------------------------------------------------
    // Internal helpers
    // ---------------------------------------------------------

    /// Build a `SubscribeRequest` for the given account pubkeys.
    /// Includes a slot subscription for chain slot
    /// synchronisation.
    fn build_account_request(
        pubkeys: &[&Pubkey],
        commitment: &CommitmentLevel,
        from_slot: Option<u64>,
    ) -> SubscribeRequest {
        let mut accounts = HashMap::new();
        accounts.insert(
            "account_subs".to_string(),
            SubscribeRequestFilterAccounts {
                account: pubkeys.iter().map(|pk| pk.to_string()).collect(),
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
            from_slot,
            ..Default::default()
        }
    }

    /// Create an account stream via the factory and insert it
    /// as the current-new stream in the [StreamMap].
    fn insert_current_new_stream(
        &mut self,
        pubkeys: &[&Pubkey],
        commitment: &CommitmentLevel,
        from_slot: Option<u64>,
    ) {
        let request =
            Self::build_account_request(pubkeys, commitment, from_slot);
        let LaserStreamWithHandle { stream, handle } =
            self.stream_factory.subscribe(request);
        self.stream_map.insert(StreamKey::CurrentNew, stream);
        self.current_new_handle = Some(handle);
    }

    /// Adds a program subscription. If the program is already
    /// subscribed, this is a no-op. Otherwise, updates the
    /// program stream to include all subscribed programs.
    pub async fn add_program_subscription(
        &mut self,
        program_id: Pubkey,
        commitment: &CommitmentLevel,
    ) -> RemoteAccountProviderResult<()> {
        if self
            .program_sub
            .as_ref()
            .is_some_and(|(subs, _)| subs.contains(&program_id))
        {
            return Ok(());
        }

        let mut subscribed_programs = self
            .program_sub
            .as_ref()
            .map(|(subs, _)| subs.clone())
            .unwrap_or_default();

        subscribed_programs.insert(program_id);

        let program_ids: Vec<&Pubkey> = subscribed_programs.iter().collect();
        let request = Self::build_program_request(&program_ids, commitment);

        if let Some((subs, handle)) = &self.program_sub {
            Self::update_subscriptions(handle, "program_subscribe", request)
                .await?;
            if let Some((subs, _)) = &mut self.program_sub {
                *subs = subscribed_programs;
            }
        } else {
            let LaserStreamWithHandle { stream, handle } =
                self.create_program_stream(&program_ids, commitment);
            self.stream_map.insert(StreamKey::Program, stream);
            self.program_sub = Some((subscribed_programs, handle));
        }

        Ok(())
    }

    /// Returns whether there are active program subscriptions.
    pub fn has_program_subscriptions(&self) -> bool {
        self.program_sub.is_some()
    }

    /// Clears all program subscriptions.
    pub fn clear_program_subscriptions(&mut self) {
        self.stream_map.remove(&StreamKey::Program);
        self.program_sub = None;
    }

    /// Build a `SubscribeRequest` for the given program IDs.
    fn build_program_request(
        program_ids: &[&Pubkey],
        commitment: &CommitmentLevel,
    ) -> SubscribeRequest {
        let mut accounts = HashMap::new();
        accounts.insert(
            "program_sub".to_string(),
            SubscribeRequestFilterAccounts {
                owner: program_ids.iter().map(|pk| pk.to_string()).collect(),
                ..Default::default()
            },
        );

        SubscribeRequest {
            accounts,
            commitment: Some((*commitment).into()),
            ..Default::default()
        }
    }

    /// Creates a subscription stream for program updates.
    fn create_program_stream(
        &self,
        program_ids: &[&Pubkey],
        commitment: &CommitmentLevel,
    ) -> LaserStreamWithHandle<S> {
        let request = Self::build_program_request(program_ids, commitment);
        self.stream_factory.subscribe(request)
    }
}

#[cfg(test)]
mod tests {
    use helius_laserstream::grpc::CommitmentLevel;
    use solana_pubkey::Pubkey;

    use super::*;
    use crate::remote_account_provider::chain_laser_actor::mock::{
        MockStreamFactory, MockStreamHandle,
    };

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

    fn create_manager() -> (
        StreamManager<MockStreamHandle, MockStreamFactory>,
        MockStreamFactory,
    ) {
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
        mgr: &StreamManager<MockStreamHandle, MockStreamFactory>,
        expected: &[Pubkey],
    ) {
        let subs = mgr.subscriptions().read();
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
    async fn subscribe_n(
        mgr: &mut StreamManager<MockStreamHandle, MockStreamFactory>,
        n: usize,
    ) -> Vec<Pubkey> {
        let pks = make_pubkeys(n);
        mgr.account_subscribe(&pks, &COMMITMENT, None)
            .await
            .unwrap();
        pks
    }

    /// Subscribe pubkeys in batches of `batch` until `total` pubkeys
    /// have been subscribed. Returns all created pubkeys.
    async fn subscribe_in_batches(
        mgr: &mut StreamManager<MockStreamHandle, MockStreamFactory>,
        total: usize,
        batch: usize,
    ) -> Vec<Pubkey> {
        let mut all = Vec::new();
        let mut remaining = total;
        while remaining > 0 {
            let n = remaining.min(batch);
            let pks = make_pubkeys(n);
            mgr.account_subscribe(&pks, &COMMITMENT, None)
                .await
                .unwrap();
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

    #[tokio::test]
    async fn test_subscribe_single_pubkey_adds_to_subscriptions() {
        let (mut mgr, factory) = create_manager();
        let pk = Pubkey::new_unique();

        mgr.account_subscribe(&[pk], &COMMITMENT, None)
            .await
            .unwrap();

        assert_subscriptions_eq(&mgr, &[pk]);

        let reqs = factory.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_request_has_exact_pubkeys(&reqs[0], &[pk]);
    }

    #[tokio::test]
    async fn test_subscribe_multiple_pubkeys_at_once() {
        let (mut mgr, factory) = create_manager();
        let pks = make_pubkeys(5);

        mgr.account_subscribe(&pks, &COMMITMENT, None)
            .await
            .unwrap();

        assert_subscriptions_eq(&mgr, &pks);

        let reqs = factory.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_request_has_exact_pubkeys(&reqs[0], &pks);
    }

    #[tokio::test]
    async fn test_subscribe_duplicate_pubkey_is_noop() {
        let (mut mgr, factory) = create_manager();
        let pk = Pubkey::new_unique();

        mgr.account_subscribe(&[pk], &COMMITMENT, None)
            .await
            .unwrap();
        let calls_after_first = factory.captured_requests().len();

        mgr.account_subscribe(&[pk], &COMMITMENT, None)
            .await
            .unwrap();

        assert_subscriptions_eq(&mgr, &[pk]);
        assert_eq!(factory.captured_requests().len(), calls_after_first);
    }

    #[tokio::test]
    async fn test_subscribe_incremental_calls_accumulate() {
        let (mut mgr, factory) = create_manager();
        let pks = make_pubkeys(3);

        mgr.account_subscribe(&[pks[0]], &COMMITMENT, None)
            .await
            .unwrap();
        mgr.account_subscribe(&[pks[1]], &COMMITMENT, None)
            .await
            .unwrap();
        mgr.account_subscribe(&[pks[2]], &COMMITMENT, None)
            .await
            .unwrap();

        assert_subscriptions_eq(&mgr, &pks);

        // First subscribe call creates the stream with just pks[0]
        let reqs = factory.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_request_has_exact_pubkeys(&reqs[0], &[pks[0]]);

        // Subsequent calls update via handle.write() which accumulates
        let handle_reqs = factory.handle_requests();
        assert!(!handle_reqs.is_empty());
        let last_handle_req = handle_reqs.last().unwrap();
        assert_request_has_exact_pubkeys(last_handle_req, &pks);
    }

    // -------------------------------------------------------------
    // 2. Current-New Stream Lifecycle
    // -------------------------------------------------------------

    #[tokio::test]
    async fn test_new_stream_created_on_first_subscribe() {
        let (mut mgr, factory) = create_manager();
        assert_eq!(mgr.account_stream_count(), 0);

        subscribe_n(&mut mgr, 1).await;

        assert_eq!(mgr.account_stream_count(), 1);
        assert_eq!(factory.active_stream_count(), 1);
    }

    #[tokio::test]
    async fn test_current_new_stream_stays_below_threshold() {
        let (mut mgr, _factory) = create_manager();
        // MAX_NEW - 1 = 4
        subscribe_in_batches(&mut mgr, 4, 2).await;

        assert_eq!(mgr.account_stream_count(), 1);
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
    }

    #[tokio::test]
    async fn test_current_new_stream_promoted_at_threshold() {
        let (mut mgr, factory) = create_manager();
        // Subscribe MAX_NEW (5) pubkeys first.
        let first_five = make_pubkeys(5);
        mgr.account_subscribe(&first_five, &COMMITMENT, None)
            .await
            .unwrap();
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);

        // Subscribe the 6th pubkey → triggers promotion.
        let sixth = Pubkey::new_unique();
        mgr.account_subscribe(&[sixth], &COMMITMENT, None)
            .await
            .unwrap();

        assert_eq!(mgr.unoptimized_old_stream_count(), 1);
        // A new current-new stream was created for the 6th pubkey.
        assert!(mgr.current_new_subs().contains(&sixth));
        // The factory received a new subscribe call for the fresh
        // current-new stream.
        let reqs = factory.captured_requests();
        assert!(reqs.len() >= 2);
    }

    #[tokio::test]
    async fn test_multiple_promotions_accumulate_unoptimized() {
        let (mut mgr, _factory) = create_manager();
        // First promotion: subscribe 6 pubkeys (exceeds MAX_NEW=5).
        subscribe_n(&mut mgr, 6).await;
        assert_eq!(mgr.unoptimized_old_stream_count(), 1);

        // Second promotion: subscribe 5 more to fill the new current,
        // then 1 more to exceed.
        subscribe_n(&mut mgr, 5).await;
        assert_eq!(mgr.unoptimized_old_stream_count(), 2);

        // Current-new stream should only hold the overflow pubkeys.
        assert!(mgr.current_new_sub_count() <= 1);
    }

    // -------------------------------------------------------------
    // 3. Optimization Trigger via MAX_OLD_UNOPTIMIZED
    // -------------------------------------------------------------

    #[tokio::test]
    async fn test_optimization_triggered_when_unoptimized_exceeds_max() {
        let (mut mgr, _factory) = create_manager();
        // MAX_OLD_UNOPTIMIZED = 3. We need 4 promotions.
        // Each promotion needs > MAX_NEW (5) pubkeys in current-new.
        // Subscribe 6 four times → 4 promotions.
        for _ in 0..3 {
            subscribe_n(&mut mgr, 6).await;
        }
        assert_eq!(mgr.unoptimized_old_stream_count(), 3);

        // 4th promotion triggers optimization.
        subscribe_n(&mut mgr, 6).await;

        // After optimization: unoptimized should be empty.
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
        // Optimized old streams should exist.
        let total_subs = mgr.subscriptions().read().len();
        let expected_optimized = total_subs.div_ceil(10); // ceil(total / MAX_OLD_OPTIMIZED)
        assert_eq!(mgr.optimized_old_stream_count(), expected_optimized,);
    }

    #[tokio::test]
    async fn test_optimization_not_triggered_below_max_unoptimized() {
        let (mut mgr, _factory) = create_manager();
        // Exactly MAX_OLD_UNOPTIMIZED (3) promotions.
        for _ in 0..3 {
            subscribe_n(&mut mgr, 6).await;
        }
        assert_eq!(mgr.unoptimized_old_stream_count(), 3);
        assert_eq!(mgr.optimized_old_stream_count(), 0);
    }

    // -------------------------------------------------------------
    // 4. Manual / Interval-Driven Optimization
    // -------------------------------------------------------------

    #[tokio::test]
    async fn test_optimize_creates_correct_number_of_optimized_streams() {
        let (mut mgr, _factory) = create_manager();
        subscribe_n(&mut mgr, 25).await;

        mgr.optimize(&COMMITMENT);

        // ceil(25 / 10) = 3
        assert_eq!(mgr.optimized_old_stream_count(), 3);
    }

    #[tokio::test]
    async fn test_optimize_clears_unoptimized_old_streams() {
        let (mut mgr, _factory) = create_manager();
        // Create several unoptimized old streams.
        for _ in 0..3 {
            subscribe_n(&mut mgr, 6).await;
        }
        assert!(mgr.unoptimized_old_stream_count() > 0);

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
        assert!(mgr.optimized_old_stream_count() > 0);
    }

    #[tokio::test]
    async fn test_optimize_resets_current_new_stream() {
        let (mut mgr, _factory) = create_manager();
        subscribe_n(&mut mgr, 8).await;

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.current_new_sub_count(), 0);
    }

    #[tokio::test]
    async fn test_optimize_excludes_unsubscribed_pubkeys() {
        let (mut mgr, factory) = create_manager();
        let pks = subscribe_n(&mut mgr, 15).await;

        // Unsubscribe 5 of them.
        let to_unsub: Vec<Pubkey> = pks[0..5].to_vec();
        mgr.account_unsubscribe(&to_unsub);

        let reqs_before = factory.captured_requests().len();
        mgr.optimize(&COMMITMENT);

        // Optimized streams should only contain the 10 remaining
        // pubkeys.
        let remaining: HashSet<String> =
            pks[5..].iter().map(|pk| pk.to_string()).collect();
        let filter_pks = all_filter_pubkeys_from(&factory, reqs_before);
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

    #[tokio::test]
    async fn test_optimize_with_zero_subscriptions() {
        let (mut mgr, _factory) = create_manager();
        let pks = subscribe_n(&mut mgr, 5).await;
        mgr.account_unsubscribe(&pks);

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.optimized_old_stream_count(), 0);
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
    }

    #[tokio::test]
    async fn test_optimize_idempotent() {
        let (mut mgr, _factory) = create_manager();
        subscribe_n(&mut mgr, 15).await;

        mgr.optimize(&COMMITMENT);
        let count_after_first = mgr.optimized_old_stream_count();

        mgr.optimize(&COMMITMENT);
        assert_eq!(mgr.optimized_old_stream_count(), count_after_first,);
    }

    // -------------------------------------------------------------
    // 5. Behavior During Optimization
    // -------------------------------------------------------------

    #[tokio::test]
    async fn test_subscribe_during_optimization_goes_to_current_new() {
        let (mut mgr, _factory) = create_manager();
        subscribe_n(&mut mgr, 20).await;

        mgr.optimize(&COMMITMENT);

        // Subscribe a new pubkey after optimization.
        let new_pk = Pubkey::new_unique();
        mgr.account_subscribe(&[new_pk], &COMMITMENT, None)
            .await
            .unwrap();

        assert!(mgr.subscriptions().read().contains(&new_pk));
        assert!(mgr.current_new_subs().contains(&new_pk));
    }

    #[tokio::test]
    async fn test_no_double_optimization_trigger() {
        let (mut mgr, _factory) = create_manager();
        // Fill up to MAX_OLD_UNOPTIMIZED.
        for _ in 0..3 {
            subscribe_n(&mut mgr, 6).await;
        }
        assert_eq!(mgr.unoptimized_old_stream_count(), 3);

        // 4th promotion triggers optimization.
        subscribe_n(&mut mgr, 6).await;
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
        let optimized_after_first = mgr.optimized_old_stream_count();

        // Now subscribe enough to exceed MAX_SUBS_IN_NEW again,
        // causing a promotion. Since optimization just ran, it should
        // NOT trigger again immediately.
        subscribe_n(&mut mgr, 6).await;
        // Unoptimized grows by 1 but no second optimization.
        assert!(mgr.unoptimized_old_stream_count() <= 1);
        assert_eq!(mgr.optimized_old_stream_count(), optimized_after_first,);
    }

    // -------------------------------------------------------------
    // 6. Unsubscribe
    // -------------------------------------------------------------

    #[tokio::test]
    async fn test_unsubscribe_removes_from_subscriptions_set() {
        let (mut mgr, _factory) = create_manager();
        let pks = make_pubkeys(3);
        mgr.account_subscribe(&pks, &COMMITMENT, None)
            .await
            .unwrap();

        mgr.account_unsubscribe(&[pks[1]]);

        assert_subscriptions_eq(&mgr, &[pks[0], pks[2]]);
    }

    #[test]
    fn test_unsubscribe_nonexistent_pubkey_is_noop() {
        let (mut mgr, _factory) = create_manager();
        let random = Pubkey::new_unique();

        mgr.account_unsubscribe(&[random]);

        assert!(mgr.subscriptions().read().is_empty());
    }

    #[tokio::test]
    async fn test_unsubscribe_already_unsubscribed_pubkey() {
        let (mut mgr, _factory) = create_manager();
        let pk = Pubkey::new_unique();
        mgr.account_subscribe(&[pk], &COMMITMENT, None)
            .await
            .unwrap();

        mgr.account_unsubscribe(&[pk]);
        mgr.account_unsubscribe(&[pk]);

        assert!(mgr.subscriptions().read().is_empty());
    }

    #[tokio::test]
    async fn test_unsubscribe_does_not_modify_streams() {
        let (mut mgr, factory) = create_manager();
        let pks = make_pubkeys(4);
        mgr.account_subscribe(&pks, &COMMITMENT, None)
            .await
            .unwrap();
        let calls_before = factory.captured_requests().len();

        mgr.account_unsubscribe(&pks[0..2]);

        // No new factory calls after unsubscribe.
        assert_eq!(factory.captured_requests().len(), calls_before);
        // Current-new subs still contain all 4 (streams not updated).
        for pk in &pks {
            assert!(mgr.current_new_subs().contains(pk));
        }
    }

    #[tokio::test]
    async fn test_unsubscribe_all_then_optimize_clears_streams() {
        let (mut mgr, _factory) = create_manager();
        // Subscribe 8 pubkeys (creates current-new + 1 unoptimized).
        let pks = subscribe_n(&mut mgr, 8).await;
        mgr.account_unsubscribe(&pks);

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.optimized_old_stream_count(), 0);
        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
    }

    #[tokio::test]
    async fn test_unsubscribe_batch() {
        let (mut mgr, factory) = create_manager();
        let pks = make_pubkeys(5);
        mgr.account_subscribe(&pks, &COMMITMENT, None)
            .await
            .unwrap();
        let calls_before = factory.captured_requests().len();

        mgr.account_unsubscribe(&[pks[0], pks[2], pks[4]]);

        assert_subscriptions_eq(&mgr, &[pks[1], pks[3]]);
        assert_eq!(factory.captured_requests().len(), calls_before);
    }

    // -------------------------------------------------------------
    // 7. Subscription Membership Check
    // -------------------------------------------------------------

    #[tokio::test]
    async fn test_is_subscribed_returns_true_for_active() {
        let (mut mgr, _factory) = create_manager();
        let pk = Pubkey::new_unique();
        mgr.account_subscribe(&[pk], &COMMITMENT, None)
            .await
            .unwrap();

        assert!(mgr.is_subscribed(&pk));
    }

    #[tokio::test]
    async fn test_is_subscribed_returns_false_after_unsubscribe() {
        let (mut mgr, _factory) = create_manager();
        let pk = Pubkey::new_unique();
        mgr.account_subscribe(&[pk], &COMMITMENT, None)
            .await
            .unwrap();
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
    // 8. Stream Count Across Generations
    // -------------------------------------------------------------

    #[tokio::test]
    async fn test_account_stream_count_includes_all_generations() {
        let (mut mgr, _factory) = create_manager();
        // Create optimized old streams.
        subscribe_n(&mut mgr, 15).await;
        mgr.optimize(&COMMITMENT);

        // Create an unoptimized old stream via promotion.
        subscribe_n(&mut mgr, 6).await;

        // Current-new also exists from the overflow pubkey.
        let count = mgr.account_stream_count();
        assert!(count > 0);
        assert_eq!(
            count,
            mgr.optimized_old_stream_count()
                + mgr.unoptimized_old_stream_count()
                + 1, // current-new
        );
    }

    #[test]
    fn test_account_stream_count_zero_when_no_subscriptions() {
        let (mgr, _factory) = create_manager();
        assert_eq!(mgr.account_stream_count(), 0);
    }

    #[tokio::test]
    async fn test_account_stream_count_after_optimize_drops_unoptimized() {
        let (mut mgr, _factory) = create_manager();
        // Create unoptimized old streams.
        for _ in 0..2 {
            subscribe_n(&mut mgr, 6).await;
        }
        assert!(mgr.unoptimized_old_stream_count() > 0);

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
        // Only optimized old streams remain (current-new is empty
        // after optimize).
        assert_eq!(
            mgr.account_stream_count(),
            mgr.optimized_old_stream_count(),
        );
    }

    // -------------------------------------------------------------
    // 9. Edge Cases and Stress
    // -------------------------------------------------------------

    #[tokio::test]
    async fn test_subscribe_exactly_at_max_subs_in_new_no_promotion() {
        let (mut mgr, _factory) = create_manager();
        // Exactly MAX_NEW (5) pubkeys — should NOT promote.
        subscribe_n(&mut mgr, 5).await;

        assert_eq!(mgr.unoptimized_old_stream_count(), 0);
        assert_eq!(mgr.account_stream_count(), 1);
    }

    #[tokio::test]
    async fn test_single_pubkey_optimization() {
        let (mut mgr, _factory) = create_manager();
        subscribe_n(&mut mgr, 1).await;

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.optimized_old_stream_count(), 1);
        assert_eq!(mgr.current_new_sub_count(), 0);
    }

    #[tokio::test]
    async fn test_subscribe_max_old_optimized_plus_one() {
        let (mut mgr, _factory) = create_manager();
        // MAX_OLD_OPTIMIZED + 1 = 11
        subscribe_n(&mut mgr, 11).await;

        mgr.optimize(&COMMITMENT);

        assert_eq!(mgr.optimized_old_stream_count(), 2);
    }

    #[tokio::test]
    async fn test_large_scale_subscribe_and_optimize() {
        let (mut mgr, factory) = create_manager();
        let pks = subscribe_n(&mut mgr, 50).await;

        let reqs_before = factory.captured_requests().len();
        mgr.optimize(&COMMITMENT);

        // ceil(50 / 10) = 5
        assert_eq!(mgr.optimized_old_stream_count(), 5);
        assert_eq!(mgr.subscriptions().read().len(), 50);
        assert_eq!(mgr.current_new_sub_count(), 0);

        // Verify the union of all optimized stream filters equals all
        // 50 pubkeys.
        let filter_pks = all_filter_pubkeys_from(&factory, reqs_before);
        assert_eq!(filter_pks.len(), 50);
        for pk in &pks {
            assert!(filter_pks.contains(&pk.to_string()));
        }
    }

    #[tokio::test]
    async fn test_interleaved_subscribe_unsubscribe_then_optimize() {
        let (mut mgr, factory) = create_manager();
        let pks = subscribe_n(&mut mgr, 20).await;
        // Unsubscribe 8 scattered.
        let unsub1: Vec<Pubkey> =
            pks.iter().step_by(2).take(8).copied().collect();
        mgr.account_unsubscribe(&unsub1);

        // Subscribe 5 new ones.
        let new_pks = subscribe_n(&mut mgr, 5).await;
        // Unsubscribe 2 of the new ones.
        mgr.account_unsubscribe(&new_pks[0..2]);

        let expected_count = 20 - 8 + 5 - 2;
        assert_eq!(mgr.subscriptions().read().len(), expected_count);

        let reqs_before = factory.captured_requests().len();
        mgr.optimize(&COMMITMENT);

        let filter_pks = all_filter_pubkeys_from(&factory, reqs_before);
        assert_eq!(filter_pks.len(), expected_count);
        // Verify unsubscribed pubkeys are absent.
        for pk in &unsub1 {
            assert!(!filter_pks.contains(&pk.to_string()));
        }
        for pk in &new_pks[0..2] {
            assert!(!filter_pks.contains(&pk.to_string()));
        }
    }

    #[tokio::test]
    async fn test_rapid_subscribe_unsubscribe_same_pubkey() {
        let (mut mgr, _factory) = create_manager();
        let pk = Pubkey::new_unique();

        mgr.account_subscribe(&[pk], &COMMITMENT, None)
            .await
            .unwrap();
        mgr.account_unsubscribe(&[pk]);
        mgr.account_subscribe(&[pk], &COMMITMENT, None)
            .await
            .unwrap();

        assert!(mgr.subscriptions().read().contains(&pk));
        assert!(mgr.current_new_subs().contains(&pk));
    }

    // -------------------------------------------------------------
    // 10. Stream Factory Interaction Verification
    // -------------------------------------------------------------

    #[tokio::test]
    async fn test_factory_called_with_correct_commitment() {
        let (mut mgr, factory) = create_manager();
        let commitment = CommitmentLevel::Finalized;
        let pk = Pubkey::new_unique();

        mgr.account_subscribe(&[pk], &commitment, None)
            .await
            .unwrap();

        let reqs = factory.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].commitment,
            Some(i32::from(CommitmentLevel::Finalized)),
        );
    }

    #[tokio::test]
    async fn test_factory_called_with_slot_filter() {
        let (mut mgr, factory) = create_manager();
        subscribe_n(&mut mgr, 1).await;

        let reqs = factory.captured_requests();
        assert!(!reqs[0].slots.is_empty());
    }

    #[tokio::test]
    async fn test_optimize_factory_calls_contain_chunked_pubkeys() {
        let (mut mgr, factory) = create_manager();
        subscribe_n(&mut mgr, 15).await;

        let reqs_before = factory.captured_requests().len();
        mgr.optimize(&COMMITMENT);

        let optimize_reqs: Vec<_> = factory
            .captured_requests()
            .into_iter()
            .skip(reqs_before)
            .collect();
        assert_eq!(optimize_reqs.len(), 2);

        let first_pks = account_pubkeys_from_request(&optimize_reqs[0]);
        let second_pks = account_pubkeys_from_request(&optimize_reqs[1]);
        assert_eq!(first_pks.len(), 10);
        assert_eq!(second_pks.len(), 5);

        // No overlap.
        assert!(first_pks.is_disjoint(&second_pks));
    }

    #[tokio::test]
    async fn test_factory_not_called_on_unsubscribe() {
        let (mut mgr, factory) = create_manager();
        subscribe_n(&mut mgr, 5).await;
        let calls_before = factory.captured_requests().len();

        let pks: Vec<Pubkey> =
            mgr.subscriptions().read().iter().take(3).copied().collect();
        mgr.account_unsubscribe(&pks);

        assert_eq!(factory.captured_requests().len(), calls_before);
    }

    // -------------------------------------------------------------
    // 11. from_slot Support
    // -------------------------------------------------------------

    #[tokio::test]
    async fn test_from_slot_set_on_subscribe_request() {
        let (mut mgr, factory) = create_manager();
        let pk = Pubkey::new_unique();

        mgr.account_subscribe(&[pk], &COMMITMENT, Some(42))
            .await
            .unwrap();

        let reqs = factory.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].from_slot, Some(42));
    }

    #[tokio::test]
    async fn test_from_slot_none_when_not_provided() {
        let (mut mgr, factory) = create_manager();
        let pk = Pubkey::new_unique();

        mgr.account_subscribe(&[pk], &COMMITMENT, None)
            .await
            .unwrap();

        let reqs = factory.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].from_slot, None);
    }

    #[tokio::test]
    async fn test_from_slot_forwarded_to_handle_write() {
        let (mut mgr, factory) = create_manager();
        let pks = make_pubkeys(2);

        // First call creates the stream.
        mgr.account_subscribe(&[pks[0]], &COMMITMENT, Some(100))
            .await
            .unwrap();
        // Second call updates via handle.write().
        mgr.account_subscribe(&[pks[1]], &COMMITMENT, Some(200))
            .await
            .unwrap();

        let handle_reqs = factory.handle_requests();
        assert_eq!(handle_reqs.len(), 1);
        assert_eq!(handle_reqs[0].from_slot, Some(200));
    }

    #[tokio::test]
    async fn test_optimize_sets_from_slot_none() {
        let (mut mgr, factory) = create_manager();
        mgr.account_subscribe(&make_pubkeys(5), &COMMITMENT, Some(42))
            .await
            .unwrap();

        let reqs_before = factory.captured_requests().len();
        mgr.optimize(&COMMITMENT);

        let optimize_reqs: Vec<_> = factory
            .captured_requests()
            .into_iter()
            .skip(reqs_before)
            .collect();
        assert!(!optimize_reqs.is_empty());
        for req in &optimize_reqs {
            assert_eq!(
                req.from_slot, None,
                "optimized streams should have from_slot=None",
            );
        }
    }

    // ---------------------------------------------------------
    // 12. next_update Stream Updates
    // ---------------------------------------------------------

    #[tokio::test]
    async fn test_next_update_receives_account_updates() {
        use helius_laserstream::grpc::SubscribeUpdate;
        use std::time::Duration;

        let (mut mgr, factory) = create_manager();
        subscribe_n(&mut mgr, 2).await;

        factory.push_update_to_stream(
            0,
            Ok(SubscribeUpdate::default()),
        );

        let result = tokio::time::timeout(
            Duration::from_millis(100),
            mgr.next_update(),
        )
        .await
        .expect("next_update timed out");

        let (source, update) = result.expect("stream ended");
        assert_eq!(source, StreamUpdateSource::Account);
        assert!(update.is_ok());
    }

    #[tokio::test]
    async fn test_next_update_receives_program_updates() {
        use helius_laserstream::grpc::SubscribeUpdate;
        use std::time::Duration;

        let (mut mgr, factory) = create_manager();
        let program_id = Pubkey::new_unique();
        mgr.add_program_subscription(program_id, &COMMITMENT)
            .await
            .unwrap();

        factory.push_update_to_stream(
            0,
            Ok(SubscribeUpdate::default()),
        );

        let result = tokio::time::timeout(
            Duration::from_millis(100),
            mgr.next_update(),
        )
        .await
        .expect("next_update timed out");

        let (source, update) = result.expect("stream ended");
        assert_eq!(source, StreamUpdateSource::Program);
        assert!(update.is_ok());
    }

    #[tokio::test]
    async fn test_next_update_receives_mixed_account_and_program()
    {
        use helius_laserstream::grpc::SubscribeUpdate;
        use std::time::Duration;

        let (mut mgr, factory) = create_manager();

        // Account stream → index 0
        subscribe_n(&mut mgr, 2).await;
        // Program stream → index 1
        let program_id = Pubkey::new_unique();
        mgr.add_program_subscription(program_id, &COMMITMENT)
            .await
            .unwrap();

        factory.push_update_to_stream(
            0,
            Ok(SubscribeUpdate::default()),
        );
        factory.push_update_to_stream(
            1,
            Ok(SubscribeUpdate::default()),
        );

        let mut sources = Vec::new();
        for _ in 0..2 {
            let result = tokio::time::timeout(
                Duration::from_millis(100),
                mgr.next_update(),
            )
            .await
            .expect("next_update timed out");

            let (source, update) =
                result.expect("stream ended");
            assert!(update.is_ok());
            sources.push(source);
        }

        assert!(
            sources.contains(&StreamUpdateSource::Account),
            "expected an Account update",
        );
        assert!(
            sources.contains(&StreamUpdateSource::Program),
            "expected a Program update",
        );
    }
}
