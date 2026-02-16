use std::collections::HashMap;

use helius_laserstream::grpc::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterSlots,
};
use solana_pubkey::Pubkey;

use super::StreamFactory;

/// Manages the creation and lifecycle of GRPC laser streams.
pub struct StreamManager<S: StreamFactory> {
    stream_factory: S,
}

impl<S: StreamFactory> StreamManager<S> {
    /// Creates a new stream manager with the given stream factory.
    pub fn new(stream_factory: S) -> Self {
        Self { stream_factory }
    }

    /// Creates a subscription stream for account updates.
    /// It includes a slot subscription for chain slot synchronization.
    /// This is not 100% cleanly separated but avoids creating another connection
    /// just for slot updates.
    /// NOTE: no slot update subscription will be created until the first
    /// accounts subscription is created.
    pub fn account_subscribe(
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

    /// Creates a subscription stream for program updates.
    pub fn program_subscribe(
        &self,
        program_ids: &[&Pubkey],
        commitment: &CommitmentLevel,
    ) -> super::LaserStream {
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
