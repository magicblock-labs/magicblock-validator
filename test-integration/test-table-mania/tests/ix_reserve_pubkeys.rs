use std::collections::HashSet;

use log::*;
use solana_pubkey::Pubkey;
use solana_sdk::{
    address_lookup_table::state::LOOKUP_TABLE_MAX_ADDRESSES, signature::Keypair,
};
use test_kit::init_logger;
use tokio::task::JoinSet;
mod utils;

// -----------------
// Fitting into single table different chunk sizes
// -----------------
macro_rules! reserve_pubkeys_in_one_table {
    ($chunk_size:expr) => {
        ::paste::paste! {
            #[tokio::test]
            async fn [<pubkeys_in_one_table_chunk_size_ $chunk_size>]() {
                reserve_pubkeys_in_one_table_in_chunks($chunk_size).await;
            }
        }
    };
}

reserve_pubkeys_in_one_table!(8);
reserve_pubkeys_in_one_table!(32);
reserve_pubkeys_in_one_table!(80);
reserve_pubkeys_in_one_table!(100);
reserve_pubkeys_in_one_table!(256);

async fn reserve_pubkeys_in_one_table_in_chunks(chunk_size: usize) {
    init_logger!();
    let authority = Keypair::new();

    let mut pubkeys = (0..LOOKUP_TABLE_MAX_ADDRESSES)
        .map(|_| Pubkey::new_unique())
        .collect::<Vec<_>>();
    pubkeys.sort();

    let table_mania = utils::setup_table_mania(&authority).await;

    for chunk in pubkeys.chunks(chunk_size) {
        debug!("Storing chunk of size: {}", chunk.len());
        let chunk_hashset = HashSet::from_iter(chunk.iter().cloned());
        table_mania
            .reserve_pubkeys(&authority, &chunk_hashset)
            .await
            .unwrap();
    }

    utils::log_active_table_addresses(&table_mania).await;

    let mut active_table_pubkeys = table_mania.active_table_pubkeys().await;
    active_table_pubkeys.sort();

    assert_eq!(table_mania.active_tables_count().await, 1);
    assert_eq!(active_table_pubkeys, pubkeys);

    let mut first_table_pubkeys = table_mania.active_tables.read().await[0]
        .get_chain_pubkeys(&table_mania.rpc_client)
        .await
        .unwrap()
        .unwrap();

    first_table_pubkeys.sort();

    assert_eq!(first_table_pubkeys, pubkeys);
}

// -----------------
// Fitting into multiple tables different chunk sizes
// -----------------
macro_rules! reserve_pubkeys_in_multiple_tables {
    ($amount:expr, $chunk_size:expr) => {
        ::paste::paste! {
            #[tokio::test]
            async fn [<store_ $amount _pubkeys_in_multiple_tables_chunk_size_ $chunk_size>]() {
                reserve_pubkeys_in_multiple_tables_in_chunks($amount, $chunk_size).await;
            }
        }
    };
}

reserve_pubkeys_in_multiple_tables!(257, 100);
reserve_pubkeys_in_multiple_tables!(512, 100);
reserve_pubkeys_in_multiple_tables!(1_000, 20);
reserve_pubkeys_in_multiple_tables!(2_100, 10);

async fn reserve_pubkeys_in_multiple_tables_in_chunks(
    amount: usize,
    chunk_size: usize,
) {
    init_logger!();
    let authority = Keypair::new();

    let pubkeys = (0..amount)
        .map(|_| Pubkey::new_unique())
        .collect::<Vec<_>>();

    let table_mania = utils::setup_table_mania(&authority).await;

    let mut join_set = JoinSet::new();
    for chunk in pubkeys.chunks(chunk_size) {
        debug!("Reserving chunk of size: {}", chunk.len());
        let chunk_hashset = HashSet::from_iter(chunk.iter().cloned());
        let table_mania = table_mania.clone();
        let authority = authority.insecure_clone();
        join_set.spawn(async move {
            table_mania
                .reserve_pubkeys(&authority, &chunk_hashset)
                .await
        });
    }
    join_set
        .join_all()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    utils::log_active_table_addresses(&table_mania).await;
    let expected_tables_count =
        (amount as f32 / LOOKUP_TABLE_MAX_ADDRESSES as f32).ceil() as usize;
    assert_eq!(
        table_mania.active_tables_count().await,
        expected_tables_count
    );
    assert_eq!(
        table_mania.active_table_pubkeys().await.len(),
        pubkeys.len()
    );
}
