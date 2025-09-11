use std::collections::HashSet;

use log::*;
use solana_pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use test_kit::init_logger;
mod utils;

#[tokio::test]
async fn test_ensure_pubkeys_table_existing_pubkey() {
    init_logger!();
    let authority = Keypair::new();
    let table_mania = utils::setup_table_mania(&authority).await;

    // Create a pubkey and reserve it first
    let pubkey = Pubkey::new_unique();
    let mut pubkeys_set = HashSet::new();
    pubkeys_set.insert(pubkey);

    // Reserve the pubkey first
    table_mania
        .reserve_pubkeys(&authority, &pubkeys_set)
        .await
        .unwrap();

    // Get refcount before ensure_pubkeys_table
    let refcount_before = table_mania.get_pubkey_refcount(&pubkey).await;
    debug!("Refcount before ensure: {:?}", refcount_before);

    // Now ensure the pubkey exists - should find it without creating a new table
    let tables_count_before = table_mania.active_tables_count().await;
    debug!("Active tables before ensure: {}", tables_count_before);

    table_mania
        .ensure_pubkeys_table(&authority, &pubkeys_set)
        .await
        .unwrap();

    // Get refcount after ensure_pubkeys_table
    let refcount_after = table_mania.get_pubkey_refcount(&pubkey).await;
    debug!("Refcount after ensure: {:?}", refcount_after);

    // Verify refcount didn't increase
    assert_eq!(
        refcount_before, refcount_after,
        "ensure_pubkeys_table should not increase refcount"
    );

    let tables_count_after = table_mania.active_tables_count().await;
    debug!("Active tables after ensure: {}", tables_count_after);

    // Should not have created a new table
    assert_eq!(tables_count_before, tables_count_after);

    // Verify the pubkey is still in a table
    let active_pubkeys = table_mania.active_table_pubkeys().await;
    assert!(active_pubkeys.contains(&pubkey));
}

#[tokio::test]
async fn test_ensure_pubkeys_table_new_pubkey() {
    init_logger!();
    let authority = Keypair::new();
    let table_mania = utils::setup_table_mania(&authority).await;

    let pubkey = Pubkey::new_unique();
    let mut pubkeys_set = HashSet::new();
    pubkeys_set.insert(pubkey);

    let tables_count_before = table_mania.active_tables_count().await;
    debug!("Active tables before ensure: {}", tables_count_before);

    // Ensure the pubkey exists - should create a new table
    table_mania
        .ensure_pubkeys_table(&authority, &pubkeys_set)
        .await
        .unwrap();

    // Get refcount after ensure_pubkeys_table
    let refcount_after = table_mania.get_pubkey_refcount(&pubkey).await;
    debug!("Refcount after ensure: {:?}", refcount_after);

    // Verify refcount is set to 1 (from the ensure_pubkeys_table call)
    assert_eq!(
        refcount_after,
        Some(1),
        "ensure_pubkeys_table should set refcount to 1 for new pubkeys"
    );

    let tables_count_after = table_mania.active_tables_count().await;
    debug!("Active tables after ensure: {}", tables_count_after);

    // Should have created a new table
    assert_eq!(tables_count_before + 1, tables_count_after);

    // Verify the pubkey is now in a table
    let active_pubkeys = table_mania.active_table_pubkeys().await;
    assert!(active_pubkeys.contains(&pubkey));
}

#[tokio::test]
async fn test_ensure_pubkeys_table_of_reserved_pubkey_doesnt_affect_ref_count()
{
    init_logger!();
    let authority = Keypair::new();
    let table_mania = utils::setup_table_mania(&authority).await;

    let pubkey = Pubkey::new_unique();
    let mut pubkeys_set = HashSet::new();
    pubkeys_set.insert(pubkey);

    // 1. Reserve the pubkey first
    table_mania
        .reserve_pubkeys(&authority, &pubkeys_set)
        .await
        .unwrap();

    // assert that table refcount is 1
    let refcount_after_reserve = table_mania.get_pubkey_refcount(&pubkey).await;
    debug!("Refcount after reserve: {:?}", refcount_after_reserve);
    assert_eq!(
        refcount_after_reserve,
        Some(1),
        "reserve_pubkeys should set refcount to 1"
    );

    // 2. Ensure pubkey
    table_mania
        .ensure_pubkeys_table(&authority, &pubkeys_set)
        .await
        .unwrap();

    // assert that table refcount is still 1
    let refcount_after_ensure = table_mania.get_pubkey_refcount(&pubkey).await;
    debug!("Refcount after ensure: {:?}", refcount_after_ensure);
    assert_eq!(
        refcount_after_ensure,
        Some(1),
        "ensure_pubkeys_table should not change refcount for reserved pubkey"
    );

    // Verify the pubkey is still in a table
    let active_pubkeys = table_mania.active_table_pubkeys().await;
    assert!(active_pubkeys.contains(&pubkey));
}

#[tokio::test]
async fn test_ensure_pubkeys_multiple_some_reserved_some_not_all_have_final_refcount_one(
) {
    init_logger!();
    let authority = Keypair::new();
    let table_mania = utils::setup_table_mania(&authority).await;

    // Create multiple pubkeys
    let reserved_pubkey1 = Pubkey::new_unique();
    let reserved_pubkey2 = Pubkey::new_unique();
    let new_pubkey1 = Pubkey::new_unique();
    let new_pubkey2 = Pubkey::new_unique();

    // Reserve some pubkeys first
    let mut reserved_pubkeys_set = HashSet::new();
    reserved_pubkeys_set.insert(reserved_pubkey1);
    reserved_pubkeys_set.insert(reserved_pubkey2);

    table_mania
        .reserve_pubkeys(&authority, &reserved_pubkeys_set)
        .await
        .unwrap();

    // Verify reserved pubkeys have refcount 1
    let reserved_refcount1 =
        table_mania.get_pubkey_refcount(&reserved_pubkey1).await;
    let reserved_refcount2 =
        table_mania.get_pubkey_refcount(&reserved_pubkey2).await;
    assert_eq!(reserved_refcount1, Some(1));
    assert_eq!(reserved_refcount2, Some(1));

    // Verify new pubkeys have no refcount yet
    let new_refcount1 = table_mania.get_pubkey_refcount(&new_pubkey1).await;
    let new_refcount2 = table_mania.get_pubkey_refcount(&new_pubkey2).await;
    assert_eq!(new_refcount1, None);
    assert_eq!(new_refcount2, None);

    // Now ensure all pubkeys (both reserved and new) in a single call
    let mut all_pubkeys_set = HashSet::new();
    all_pubkeys_set.insert(reserved_pubkey1);
    all_pubkeys_set.insert(reserved_pubkey2);
    all_pubkeys_set.insert(new_pubkey1);
    all_pubkeys_set.insert(new_pubkey2);

    table_mania
        .ensure_pubkeys_table(&authority, &all_pubkeys_set)
        .await
        .unwrap();

    // Verify all pubkeys have refcount 1 after ensure
    let final_refcount1 =
        table_mania.get_pubkey_refcount(&reserved_pubkey1).await;
    let final_refcount2 =
        table_mania.get_pubkey_refcount(&reserved_pubkey2).await;
    let final_refcount3 = table_mania.get_pubkey_refcount(&new_pubkey1).await;
    let final_refcount4 = table_mania.get_pubkey_refcount(&new_pubkey2).await;

    debug!("Final refcount reserved_pubkey1: {:?}", final_refcount1);
    debug!("Final refcount reserved_pubkey2: {:?}", final_refcount2);
    debug!("Final refcount new_pubkey1: {:?}", final_refcount3);
    debug!("Final refcount new_pubkey2: {:?}", final_refcount4);

    assert_eq!(
        final_refcount1,
        Some(1),
        "Reserved pubkey should maintain refcount 1"
    );
    assert_eq!(
        final_refcount2,
        Some(1),
        "Reserved pubkey should maintain refcount 1"
    );
    assert_eq!(
        final_refcount3,
        Some(1),
        "New pubkey should have refcount 1"
    );
    assert_eq!(
        final_refcount4,
        Some(1),
        "New pubkey should have refcount 1"
    );

    // Verify all pubkeys are in active tables
    let active_pubkeys = table_mania.active_table_pubkeys().await;
    assert!(active_pubkeys.contains(&reserved_pubkey1));
    assert!(active_pubkeys.contains(&reserved_pubkey2));
    assert!(active_pubkeys.contains(&new_pubkey1));
    assert!(active_pubkeys.contains(&new_pubkey2));
}
