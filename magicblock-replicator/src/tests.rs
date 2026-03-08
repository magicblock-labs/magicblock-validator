//! Tests suite for replication protocol.

use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use tokio::net::{TcpListener, TcpStream};

use crate::proto::{Block, FailoverSignal, Message, SuperBlock, Transaction};

// =============================================================================
// Wire Format Tests - catch serialization/protocol changes
// =============================================================================

#[test]
fn variant_order_stability() {
    // Bincode encodes enum discriminant as variant index.
    // Reordering enum variants silently breaks wire compatibility.
    let cases: [(Message, u32); 4] = [
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
                hash: solana_hash::Hash::default(),
                timestamp: 42,
            }),
            1,
        ),
        (
            Message::SuperBlock(SuperBlock {
                blocks: 0,
                transactions: 0,
                checksum: 0,
            }),
            2,
        ),
        (
            Message::Failover(FailoverSignal::new(0, &Keypair::new())),
            3,
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
fn signed_message_roundtrip() {
    // Signed messages (handshake, failover) have complex serialization.
    // Unsigned messages are trivial and covered by variant_order_stability.
    let kp = Keypair::new();

    let cases = vec![
        Message::Failover(FailoverSignal::new(77777, &kp)),
        Message::Transaction(Transaction {
            slot: 54321,
            index: 42,
            payload: (0..255).collect(),
        }),
    ];

    for msg in cases {
        let encoded = bincode::serialize(&msg).unwrap();
        let decoded: Message = bincode::deserialize(&encoded).unwrap();
        assert_eq!(bincode::serialize(&decoded).unwrap(), encoded);
    }
}

// =============================================================================
// Signature Verification Tests - catch crypto/auth bugs
// =============================================================================

#[test]
fn failover_signal_verification() {
    let kp = Keypair::new();
    let signal = FailoverSignal::new(99999, &kp);

    // Correct identity verifies
    assert!(signal.verify(kp.pubkey()));

    // Wrong identity fails
    assert!(
        !signal.verify(Pubkey::new_unique()),
        "wrong identity should fail"
    );
}
