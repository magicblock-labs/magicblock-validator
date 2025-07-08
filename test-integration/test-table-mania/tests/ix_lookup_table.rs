use log::*;

use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{
    find_open_tables, LookupTableRc, TableManiaComputeBudgets,
    CREATE_AND_EXTEND_TABLE_CUS, DEACTIVATE_TABLE_CUS, EXTEND_TABLE_CUS,
    MAX_ENTRIES_AS_PART_OF_EXTEND,
};
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
    budgets: &TableManiaComputeBudgets,
) -> (MagicblockRpcClient, LookupTableRc) {
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
    let lookup_table = match LookupTableRc::init(
        &rpc_client,
        validator_auth,
        latest_slot,
        sub_slot,
        pubkeys,
        &budgets.init,
    )
    .await
    {
        Ok(tbl) => tbl,
        Err(err) => {
            if let Some(sig) = err.signature() {
                let logs = rpc_client
                    .get_transaction_logs(&sig, None)
                    .await
                    .unwrap()
                    .unwrap();
                let cus = rpc_client
                    .get_transaction_cus(&sig, None)
                    .await
                    .unwrap()
                    .unwrap();
                panic!(
                    "Failed to init lookup table: {err:?} with logs: {logs:#?} used {cus} CUs"
                );
            }
            panic!("Failed to init lookup table: {err:?}");
        }
    };
    let init_cus = rpc_client
        .get_transaction_cus(&lookup_table.init_signature().unwrap(), None)
        .await
        .unwrap()
        .unwrap();
    debug!("Lookup table initialized using {init_cus} CUs");
    (rpc_client, lookup_table)
}

async fn get_table_meta(
    rpc_client: &MagicblockRpcClient,
    lookup_table: &LookupTableRc,
) -> LookupTableMeta {
    lookup_table
        .get_meta(rpc_client)
        .await
        .unwrap()
        .expect("Table not found")
}

async fn get_table_addresses(
    rpc_client: &MagicblockRpcClient,
    lookup_table: &LookupTableRc,
) -> Vec<Pubkey> {
    let mut keys = lookup_table
        .get_chain_pubkeys(rpc_client)
        .await
        .unwrap()
        .expect("Table not found");
    keys.sort();
    keys
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
    let mut pubkeys = vec![0; 10]
        .into_iter()
        .map(|_| Pubkey::new_unique())
        .collect::<Vec<_>>();
    pubkeys.sort();

    let budgets = TableManiaComputeBudgets::default();

    // Init table
    let (rpc_client, mut lookup_table) =
        setup_lookup_table(&validator_auth, &pubkeys[0..5], &budgets).await;
    let creation_slot = lookup_table.creation_slot().unwrap();
    let meta = get_table_meta(&rpc_client, &lookup_table).await;

    assert_eq!(meta.authority, Some(lookup_table.derived_auth().pubkey()));
    assert_eq!(meta.deactivation_slot, u64::MAX);
    let mut keys = lookup_table
        .pubkeys()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, pubkeys[0..5]);
    assert_eq!(
        get_table_addresses(&rpc_client, &lookup_table).await,
        pubkeys[0..5]
    );
    debug!("{}", lookup_table);

    // Extend table
    debug!("Extending table ...");
    lookup_table
        .extend(
            &rpc_client,
            &validator_auth,
            &pubkeys[5..10],
            &budgets.extend,
        )
        .await
        .unwrap();
    let mut keys = lookup_table
        .pubkeys()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, pubkeys[0..10]);
    assert_eq!(
        get_table_addresses(&rpc_client, &lookup_table).await,
        pubkeys[0..10]
    );

    // Deactivate table
    debug!("Deactivating table ...");
    lookup_table
        .deactivate(&rpc_client, &validator_auth, &budgets.deactivate)
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

#[tokio::test]
async fn test_lookup_table_ixs_cus_per_pubkey() {
    init_logger!();

    let budgets = TableManiaComputeBudgets::default();

    let validator_auth = Keypair::new();
    let init_pubkeys = vec![0; MAX_ENTRIES_AS_PART_OF_EXTEND as usize]
        .into_iter()
        .map(|_| Pubkey::new_unique())
        .collect::<Vec<_>>();

    let extend_pubkeys = vec![0; 10_000]
        .into_iter()
        .map(|_| Pubkey::new_unique())
        .collect::<Vec<_>>();

    let mut extend_idx = 0;
    for i in 1..init_pubkeys.len() {
        let (rpc_client, mut lookup_table) =
            setup_lookup_table(&validator_auth, &init_pubkeys[0..=i], &budgets)
                .await;

        let init_sig = lookup_table.init_signature().unwrap();
        let cus = get_tx_cus(&rpc_client, &init_sig).await;

        debug!("Create for {i:03} {cus:04}CUs");
        assert_eq!(cus, CREATE_AND_EXTEND_TABLE_CUS as u64);

        lookup_table
            .extend(
                &rpc_client,
                &validator_auth,
                &extend_pubkeys[extend_idx..=extend_idx + i],
                &budgets.extend,
            )
            .await
            .unwrap();
        extend_idx += i;

        let extend_sig =
            *lookup_table.extend_signatures().unwrap().last().unwrap();
        let cus = get_tx_cus(&rpc_client, &extend_sig).await;
        debug!("Extend for {i:03} CUs  {cus:04}CUs");
        assert_eq!(cus, EXTEND_TABLE_CUS as u64);

        lookup_table
            .deactivate(&rpc_client, &validator_auth, &budgets.deactivate)
            .await
            .unwrap();

        let cus = get_tx_cus(
            &rpc_client,
            &lookup_table.deactivate_signature().unwrap(),
        )
        .await;
        debug!("Deactivate table {cus:03}CUs");
        assert_eq!(cus, DEACTIVATE_TABLE_CUS as u64);

        #[cfg(feature = "test_table_close")]
        {
            // Testing close takes a long time and is always the same instruction,
            // thus we only perform this test once
            if i == 1 {
                eprintln!(
                    "Waiting for table to deactivate for about 2.5 min ..."
                );
                while !lookup_table.is_deactivated(&rpc_client, None).await {
                    utils::sleep_millis(5_000).await;
                }
                let (is_closed, close_sig) = lookup_table
                    .close(&rpc_client, &validator_auth, None)
                    .await
                    .unwrap();
                assert!(is_closed);
                let cus = get_tx_cus(&rpc_client, &close_sig.unwrap()).await;
                debug!("Close table {cus:03}CUs",);
                assert_eq!(cus, CLOSE_TABLE_CUS as u64);
            }
        }
    }
}

async fn get_tx_cus(
    rpc_client: &MagicblockRpcClient,
    sig: &solana_sdk::signature::Signature,
) -> u64 {
    let tx = rpc_client.get_transaction(sig, None).await.unwrap();
    tx.transaction
        .meta
        .as_ref()
        .unwrap()
        .compute_units_consumed
        .clone()
        .unwrap()
}
