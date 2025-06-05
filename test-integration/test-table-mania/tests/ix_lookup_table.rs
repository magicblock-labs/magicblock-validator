use log::*;

use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{find_open_tables, LookupTable};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    address_lookup_table::state::LookupTableMeta, clock::Slot,
    commitment_config::CommitmentConfig, native_token::LAMPORTS_PER_SOL,
    signature::Keypair, signer::Signer,
};
use test_tools_core::init_logger;

mod utils;

pub async fn setup_lookup_table(
    validator_auth: &Keypair,
    pubkeys: &[Pubkey],
) -> (MagicblockRpcClient, LookupTable) {
    let rpc_client = {
        let client = RpcClient::new_with_commitment(
            "http://localhost:7799".to_string(),
            CommitmentConfig::confirmed(),
        );
        MagicblockRpcClient::from(client)
    };
    rpc_client
        .request_airdrop(&validator_auth.pubkey(), 777 * LAMPORTS_PER_SOL)
        .await
        .unwrap();

    let latest_slot = rpc_client.get_slot().await.unwrap();
    let sub_slot = 0;
    let reqid = 0;
    let lookup_table = LookupTable::init(
        &rpc_client,
        validator_auth,
        latest_slot,
        sub_slot,
        pubkeys,
        reqid,
    )
    .await
    .unwrap();
    (rpc_client, lookup_table)
}

async fn get_table_meta(
    rpc_client: &MagicblockRpcClient,
    lookup_table: &LookupTable,
) -> LookupTableMeta {
    lookup_table
        .get_meta(rpc_client)
        .await
        .unwrap()
        .expect("Table not found")
}

async fn get_table_addresses(
    rpc_client: &MagicblockRpcClient,
    lookup_table: &LookupTable,
) -> Vec<Pubkey> {
    lookup_table
        .get_chain_pubkeys(rpc_client)
        .await
        .unwrap()
        .expect("Table not found")
}

async fn get_open_tables(
    rpc_client: &MagicblockRpcClient,
    authority: &Keypair,
    start_slot: Slot,
) -> Vec<Pubkey> {
    let end_slot = rpc_client.get_slot().await.unwrap();
    find_open_tables(rpc_client, authority, start_slot, end_slot, 10)
        .await
        .unwrap()
        .tables
}

#[tokio::test]
async fn test_create_fetch_and_close_lookup_table() {
    init_logger!();

    let validator_auth = Keypair::new();
    let pubkeys = vec![0; 10]
        .into_iter()
        .map(|_| Pubkey::new_unique())
        .collect::<Vec<_>>();

    // Init table
    let (rpc_client, mut lookup_table) =
        setup_lookup_table(&validator_auth, &pubkeys[0..5]).await;
    let creation_slot = lookup_table.creation_slot().unwrap();
    let meta = get_table_meta(&rpc_client, &lookup_table).await;

    assert_eq!(meta.authority, Some(lookup_table.derived_auth().pubkey()));
    assert_eq!(meta.deactivation_slot, u64::MAX);
    assert_eq!(lookup_table.pubkeys().unwrap(), pubkeys[0..5]);
    assert_eq!(
        get_table_addresses(&rpc_client, &lookup_table).await,
        pubkeys[0..5]
    );
    debug!("{}", lookup_table);

    // Extend table
    let reqid = 0;
    debug!("Extending table ...");
    lookup_table
        .extend(&rpc_client, &validator_auth, &pubkeys[5..10], reqid)
        .await
        .unwrap();
    assert_eq!(lookup_table.pubkeys().unwrap(), pubkeys[0..10]);
    assert_eq!(
        get_table_addresses(&rpc_client, &lookup_table).await,
        pubkeys[0..10]
    );

    // Deactivate table
    debug!("Deactivating table ...");
    lookup_table
        .deactivate(&rpc_client, &validator_auth)
        .await
        .unwrap();

    let meta = get_table_meta(&rpc_client, &lookup_table).await;
    assert_eq!(meta.authority, Some(lookup_table.derived_auth().pubkey()));
    assert_ne!(meta.deactivation_slot, u64::MAX);
    assert!(!lookup_table.is_deactivated(&rpc_client, None).await);

    assert_eq!(
        get_open_tables(&rpc_client, &validator_auth, creation_slot)
            .await
            .len(),
        1
    );

    #[cfg(not(feature = "test_table_close"))]
    eprintln!("SKIP: close table");

    #[cfg(feature = "test_table_close")]
    {
        // Wait for deactivation and close table
        debug!("{}", lookup_table);

        eprintln!("Waiting for table to deactivate for about 2.5 min ...");
        while !lookup_table.is_deactivated(&rpc_client, None).await {
            utils::sleep_millis(5_000).await;
        }
        lookup_table
            .close(&rpc_client, &validator_auth, None)
            .await
            .unwrap();
        assert!(lookup_table.is_closed(&rpc_client).await.unwrap());

        assert_eq!(
            get_open_tables(&rpc_client, &validator_auth, creation_slot)
                .await
                .len(),
            0
        );
    }
}
