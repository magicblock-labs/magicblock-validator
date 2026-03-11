//! Tests for the replication protocol.

use solana_hash::Hash;

use crate::proto::{Block, Message, SuperBlock, Transaction};

// =============================================================================
// Wire Format Tests - catch serialization/protocol changes
// =============================================================================

#[test]
fn variant_order_stability() {
    // Bincode encodes enum discriminant as variant index.
    // Reordering enum variants silently breaks wire compatibility.
    let cases: [(Message, u32); 3] = [
        (
            Message::Transaction(Transaction {
                slot: 0,
                index: 0,
                payload: vec![],
            }),
            0,
        ),
        (
            Message::Block(Block {
                slot: 0,
                hash: Hash::default(),
                timestamp: 42,
            }),
            1,
        ),
        (
            Message::SuperBlock(SuperBlock {
                slot: 0,
                transactions: 0,
                checksum: 0,
            }),
            2,
        ),
    ];

    for (msg, expected_idx) in cases {
        let encoded = bincode::serialize(&msg).unwrap();
        let actual_idx = u32::from_le_bytes([
            encoded[0], encoded[1], encoded[2], encoded[3],
        ]);
        assert_eq!(
            actual_idx, expected_idx,
            "variant index changed - this breaks wire compatibility!"
        );
    }
}

#[test]
fn message_roundtrip() {
    let cases = vec![
        Message::Transaction(Transaction {
            slot: 54321,
            index: 42,
            payload: (0..255).collect(),
        }),
        Message::Block(Block {
            slot: 12345,
            hash: Hash::new_unique(),
            timestamp: 1700000000,
        }),
        Message::SuperBlock(SuperBlock {
            slot: 99999,
            transactions: 50000,
            checksum: 0xDEADBEEF,
        }),
    ];

    for msg in cases {
        let encoded = bincode::serialize(&msg).unwrap();
        let decoded: Message = bincode::deserialize(&encoded).unwrap();
        assert_eq!(bincode::serialize(&decoded).unwrap(), encoded);
    }
}
