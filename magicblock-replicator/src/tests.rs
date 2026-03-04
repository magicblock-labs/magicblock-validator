//! Tests suite for replication protocol.

use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use tokio::net::{TcpListener, TcpStream};

use crate::{
    proto::{
        Block, FailoverSignal, HandshakeRequest, HandshakeResponse, Message,
        SuperBlock, Transaction,
    },
    tcp::split,
};

// =============================================================================
// Wire Format Tests - catch serialization/protocol changes
// =============================================================================

#[test]
fn variant_order_stability() {
    // Bincode encodes enum discriminant as variant index.
    // Reordering enum variants silently breaks wire compatibility.
    let cases: [(Message, u32); 6] = [
        (
            Message::HandshakeReq(HandshakeRequest::new(0, &Keypair::new())),
            0,
        ),
        (
            Message::HandshakeResp(HandshakeResponse::new(
                Ok(0),
                &Keypair::new(),
            )),
            1,
        ),
        (
            Message::Transaction(Transaction {
                slot: 0,
                index: 0,
                payload: vec![],
            }),
            2,
        ),
        (
            Message::Block(Block {
                slot: 0,
                hash: solana_hash::Hash::default(),
            }),
            3,
        ),
        (
            Message::SuperBlock(SuperBlock {
                blocks: 0,
                transactions: 0,
                checksum: 0,
            }),
            4,
        ),
        (
            Message::Failover(FailoverSignal::new(0, &Keypair::new())),
            5,
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
        Message::HandshakeReq(HandshakeRequest::new(12345, &kp)),
        Message::HandshakeResp(HandshakeResponse::new(Ok(99999), &kp)),
        Message::HandshakeResp(HandshakeResponse::new(
            Err(crate::error::Error::ConnectionClosed),
            &kp,
        )),
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
fn handshake_tampering_detected() {
    let kp = Keypair::new();
    let req = HandshakeRequest::new(12345, &kp);

    // Valid signature works
    assert!(req.verify());

    // Tampered identity fails
    let mut tampered = req.clone();
    tampered.identity = Pubkey::new_unique();
    assert!(!tampered.verify(), "tampered identity should fail");

    // Tampered slot fails (slot is at offset 4 after version u32)
    let mut bytes = bincode::serialize(&req).unwrap();
    bytes[4..12].copy_from_slice(&99999u64.to_le_bytes());
    let decoded: HandshakeRequest = bincode::deserialize(&bytes).unwrap();
    assert!(!decoded.verify(), "tampered slot should fail");
}

#[test]
fn handshake_response_signing() {
    // Success and error paths sign different data - both must verify.
    let kp = Keypair::new();

    let success = HandshakeResponse::new(Ok(5000), &kp);
    assert!(success.verify(), "success response should verify");

    let error =
        HandshakeResponse::new(Err(crate::error::Error::ConnectionClosed), &kp);
    assert!(error.verify(), "error response should verify");
}

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

// =============================================================================
// TCP Transport Tests - catch framing/connection bugs
// =============================================================================

#[tokio::test]
async fn bidirectional_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let client = TcpStream::connect(addr).await.unwrap();
    let (server, _) = listener.accept().await.unwrap();

    let (mut client_tx, mut client_rx) = split(client);
    let (mut server_tx, mut server_rx) = split(server);

    // Client -> Server: handshake request
    let kp = Keypair::new();
    client_tx
        .send(&Message::HandshakeReq(HandshakeRequest::new(1000, &kp)))
        .await
        .unwrap();

    let req = match server_rx.recv().await.unwrap() {
        Message::HandshakeReq(r) => r,
        _ => panic!("expected HandshakeReq"),
    };
    assert!(req.verify());
    assert_eq!(req.start_slot, 1000);

    // Server -> Client: handshake response
    server_tx
        .send(&Message::HandshakeResp(HandshakeResponse::new(
            Ok(1000),
            &Keypair::new(),
        )))
        .await
        .unwrap();

    let resp = match client_rx.recv().await.unwrap() {
        Message::HandshakeResp(r) => r,
        _ => panic!("expected HandshakeResp"),
    };
    assert!(resp.verify());
    assert_eq!(resp.result, Ok(1000u64));
}

#[tokio::test]
async fn message_ordering_over_stream() {
    // Tests that TCP framing preserves message boundaries and order.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let client = TcpStream::connect(addr).await.unwrap();
    let (server, _) = listener.accept().await.unwrap();

    let (mut tx, _) = split(client);
    let (_, mut rx) = split(server);

    // Send mixed message types
    for i in 0..10 {
        tx.send(&Message::Block(Block {
            slot: i,
            hash: solana_hash::Hash::new_unique(),
        }))
        .await
        .unwrap();
    }

    // Verify order is preserved
    for expected in 0..10 {
        match rx.recv().await.unwrap() {
            Message::Block(b) => assert_eq!(b.slot, expected),
            _ => panic!("expected Block"),
        }
    }
}

#[tokio::test]
async fn large_payload() {
    // Tests frame handling for messages larger than TCP buffer.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let client = TcpStream::connect(addr).await.unwrap();
    let (server, _) = listener.accept().await.unwrap();

    let (mut tx, _) = split(client);
    let (_, mut rx) = split(server);

    let payload = vec![0xAB; 1024 * 1024]; // 1MB
    tx.send(&Message::Transaction(Transaction {
        slot: 0,
        index: 0,
        payload: payload.clone(),
    }))
    .await
    .unwrap();

    match rx.recv().await.unwrap() {
        Message::Transaction(t) => {
            assert_eq!(t.payload, payload);
        }
        _ => panic!("expected Transaction"),
    }
}

#[tokio::test]
async fn all_message_types_over_wire() {
    // Tests encoder→TCP→decoder path for all 6 message types.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let client = TcpStream::connect(addr).await.unwrap();
    let (server, _) = listener.accept().await.unwrap();

    let (mut tx, _) = split(client);
    let (_, mut rx) = split(server);

    let kp = Keypair::new();
    let messages: Vec<Message> = vec![
        Message::HandshakeReq(HandshakeRequest::new(12345, &kp)),
        Message::HandshakeResp(HandshakeResponse::new(Ok(67890), &kp)),
        Message::Transaction(Transaction {
            slot: 100,
            index: 5,
            payload: vec![0xDE, 0xAD, 0xBE, 0xEF],
        }),
        Message::Block(Block {
            slot: 200,
            hash: solana_hash::Hash::new_unique(),
        }),
        Message::SuperBlock(SuperBlock {
            blocks: 1000,
            transactions: 50000,
            checksum: 0xCAFEBABE,
        }),
        Message::Failover(FailoverSignal::new(99999, &kp)),
    ];

    for msg in &messages {
        tx.send(msg).await.unwrap();
    }

    for expected in &messages {
        let received = rx.recv().await.unwrap();
        // Compare serialized form to catch any encoding differences
        assert_eq!(
            bincode::serialize(&received).unwrap(),
            bincode::serialize(expected).unwrap(),
            "wire roundtrip mismatch"
        );
    }
}
