use std::collections::HashSet;

use solana_pubkey::Pubkey;
use solana_sdk::signature::Keypair;
mod utils;

#[tokio::test]
async fn test_single_table_two_requests_with_overlapping_pubkeys() {
    utils::init_logger();

    let authority = Keypair::new();
    let table_mania = utils::setup_table_mania(&authority).await;

    let pubkeys_req1 = (0..10)
        .map(|idx| Pubkey::from([idx; 32]))
        .collect::<HashSet<_>>();
    let pubkeys_req2 = (6..10)
        .map(|idx| Pubkey::from([idx; 32]))
        .collect::<HashSet<_>>();

    table_mania
        .reserve_pubkeys(&authority, &pubkeys_req1)
        .await
        .unwrap();
    table_mania
        .reserve_pubkeys(&authority, &pubkeys_req2)
        .await
        .unwrap();

    utils::log_active_table_addresses(&table_mania).await;

    assert_eq!(table_mania.active_tables_count().await, 1);
    assert_eq!(table_mania.released_tables_count().await, 0);

    // All of req2 pubkeys are also contained in req1
    // However when we release all req1 pubkeys the table should not be released
    // yet since req2 still needs them

    table_mania.release_pubkeys(&pubkeys_req1).await;
    assert_eq!(table_mania.active_tables_count().await, 1);
    assert_eq!(table_mania.released_tables_count().await, 0);

    // Now releasing req2 pubkeys should release the table
    table_mania.release_pubkeys(&pubkeys_req2).await;
    assert_eq!(table_mania.active_tables_count().await, 0);
    assert_eq!(table_mania.released_tables_count().await, 1);

    utils::close_released_tables(&table_mania).await
}

#[tokio::test]
async fn test_two_table_three_requests_with_one_overlapping_pubkey() {
    utils::init_logger();

    let authority = Keypair::new();
    let table_mania = utils::setup_table_mania(&authority).await;

    let common_pubkey = Pubkey::new_unique();
    let mut pubkeys_req1 = (0..300)
        .map(|_| Pubkey::new_unique())
        .collect::<HashSet<_>>();

    // The common pubkey will be stored in the second table
    pubkeys_req1.insert(common_pubkey);

    let pubkeys_req2 = HashSet::from([common_pubkey]);
    let pubkeys_req3 = HashSet::from([common_pubkey]);

    table_mania
        .reserve_pubkeys(&authority, &pubkeys_req1)
        .await
        .unwrap();
    table_mania
        .reserve_pubkeys(&authority, &pubkeys_req2)
        .await
        .unwrap();
    table_mania
        .reserve_pubkeys(&authority, &pubkeys_req3)
        .await
        .unwrap();

    utils::log_active_table_addresses(&table_mania).await;

    assert_eq!(table_mania.active_tables_count().await, 2);
    assert_eq!(table_mania.released_tables_count().await, 0);

    // Releasing req2 should not release any table since it only
    // has the common pubkey
    table_mania.release_pubkeys(&pubkeys_req2).await;
    assert_eq!(table_mania.active_tables_count().await, 2);
    assert_eq!(table_mania.released_tables_count().await, 0);

    // Releasing req1 should only release the first table since the
    // second table has the common pubkey
    table_mania.release_pubkeys(&pubkeys_req1).await;
    assert_eq!(table_mania.active_tables_count().await, 1);
    assert_eq!(table_mania.released_tables_count().await, 1);

    // Releasing req3 frees the common pubkey and thus allows the
    // second table to be released
    table_mania.release_pubkeys(&pubkeys_req3).await;
    assert_eq!(table_mania.active_tables_count().await, 0);
    assert_eq!(table_mania.released_tables_count().await, 2);

    utils::close_released_tables(&table_mania).await
}
