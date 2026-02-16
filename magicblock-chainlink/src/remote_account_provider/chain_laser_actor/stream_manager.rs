use std::collections::{HashMap, HashSet};

use helius_laserstream::grpc::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterSlots,
};
use solana_pubkey::Pubkey;

use super::{LaserStream, StreamFactory};

/// Manages the creation and lifecycle of GRPC laser streams.
pub struct StreamManager<S: StreamFactory> {
    stream_factory: S,
    /// Active streams for program subscriptions
    program_subscriptions: Option<(HashSet<Pubkey>, LaserStream)>,
    /// Active streams for account subscriptions
    account_streams: Vec<LaserStream>,
}

impl<S: StreamFactory> StreamManager<S> {
    /// Creates a new stream manager with the given stream factory.
    pub fn new(stream_factory: S) -> Self {
        Self {
            stream_factory,
            program_subscriptions: None,
            account_streams: Vec::new(),
        }
    }

    /// Creates a subscription stream for account updates.
    /// It includes a slot subscription for chain slot synchronization.
    /// This is not 100% cleanly separated but avoids creating another connection
    /// just for slot updates.
    /// NOTE: no slot update subscription will be created until the first
    /// accounts subscription is created.
    #[allow(unused)]
    pub fn account_subscribe(
        &mut self,
        pubkeys: &[&Pubkey],
        commitment: &CommitmentLevel,
        idx: usize,
        from_slot: Option<u64>,
    ) {
        let stream =
            self.account_subscribe_old(pubkeys, commitment, idx, from_slot);
        self.account_streams.push(stream);
    }

    /// Creates a subscription stream for account updates.
    /// It includes a slot subscription for chain slot synchronization.
    /// This is not 100% cleanly separated but avoids creating another connection
    /// just for slot updates.
    /// NOTE: no slot update subscription will be created until the first
    /// accounts subscription is created.
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
            // NOTE: triton does not support backfilling and we could not verify this with
            // helius due to being rate limited.
            from_slot,
            ..Default::default()
        };
        self.stream_factory.subscribe(request)
    }

    /// Adds a program subscription. If the program is already subscribed,
    /// this is a no-op. Otherwise, recreates the program stream to include
    /// all subscribed programs.
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

    /// Returns a mutable reference to the program subscriptions stream
    /// (if any) for polling in the actor loop.
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
