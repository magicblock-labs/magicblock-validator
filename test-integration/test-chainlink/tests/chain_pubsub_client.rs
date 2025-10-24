use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use magicblock_chainlink::{
    remote_account_provider::{
        chain_pubsub_client::{ChainPubsubClient, ChainPubsubClientImpl},
        SubscriptionUpdate,
    },
    testing::{
        init_logger,
        utils::{airdrop, random_pubkey, PUBSUB_URL, RPC_URL},
    },
};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    clock::Clock, commitment_config::CommitmentConfig, sysvar::clock,
};
use tokio::{sync::mpsc, task};

async fn setup() -> (ChainPubsubClientImpl, mpsc::Receiver<SubscriptionUpdate>)
{
    init_logger();
    let client = ChainPubsubClientImpl::try_new_from_url(
        PUBSUB_URL,
        CommitmentConfig::confirmed(),
    )
    .await
    .unwrap();
    let updates = client.take_updates();
    (client, updates)
}

fn updates_to_lamports(updates: &[SubscriptionUpdate]) -> Vec<u64> {
    updates
        .iter()
        .map(|update| {
            let res = &update.rpc_response;
            res.value.lamports
        })
        .collect()
}

macro_rules! lamports {
    ($received_updates:ident, $pubkey:ident) => {
        $received_updates
            .lock()
            .unwrap()
            .get(&$pubkey)
            .map(|x| updates_to_lamports(x))
    };
}

fn updates_total_len(
    updates: &Mutex<HashMap<Pubkey, Vec<SubscriptionUpdate>>>,
) -> usize {
    updates
        .lock()
        .unwrap()
        .values()
        .map(|updates| updates.len())
        .sum()
}

async fn sleep_millis(millis: u64) {
    tokio::time::sleep(tokio::time::Duration::from_millis(millis)).await;
}

async fn wait_for_updates(
    updates: &Mutex<HashMap<Pubkey, Vec<SubscriptionUpdate>>>,
    starting_len: usize,
    amount: usize,
) {
    while updates_total_len(updates) < starting_len + amount {
        sleep_millis(100).await;
    }
}

#[tokio::test]
async fn ixtest_chain_pubsub_client_clock() {
    const ITER: usize = 3;

    let (client, mut updates) = setup().await;

    client.subscribe(clock::ID).await.unwrap();
    let mut received_updates = vec![];
    while let Some(update) = updates.recv().await {
        received_updates.push(update);
        if received_updates.len() == ITER {
            break;
        }
    }
    client.shutdown().await;

    assert_eq!(received_updates.len(), ITER);

    let mut last_slot = None;
    for update in received_updates {
        let clock_data = update.rpc_response.value.data.decode().unwrap();
        let clock_value = bincode::deserialize::<Clock>(&clock_data).unwrap();
        // We show as part of this test that the context slot always matches
        // the clock slot which allows us to save on parsing in production since
        // we can just use the context slot instead of parsing the clock data.
        assert_eq!(update.rpc_response.context.slot, clock_value.slot);
        if let Some(last_slot) = last_slot {
            assert!(clock_value.slot > last_slot);
        } else {
            last_slot = Some(clock_value.slot);
        }
    }
}

#[tokio::test]
async fn ixtest_chain_pubsub_client_airdropping() {
    let rpc_client = RpcClient::new_with_commitment(
        RPC_URL.to_string(),
        CommitmentConfig::confirmed(),
    );
    let (client, mut updates) = setup().await;

    let received_updates = {
        let map = HashMap::new();
        Arc::new(Mutex::new(map))
    };

    task::spawn({
        let received_updates = received_updates.clone();
        async move {
            while let Some(update) = updates.recv().await {
                let mut map = received_updates.lock().unwrap();
                map.entry(update.pubkey)
                    .or_insert_with(Vec::new)
                    .push(update);
            }
        }
    });

    let pubkey1 = random_pubkey();
    let pubkey2 = random_pubkey();

    {
        let len = updates_total_len(&received_updates);

        client.subscribe(pubkey1).await.unwrap();
        airdrop(&rpc_client, &pubkey1, 1_000_000).await;
        airdrop(&rpc_client, &pubkey2, 1_000_000).await;

        wait_for_updates(&received_updates, len, 1).await;

        let lamports1 =
            lamports!(received_updates, pubkey1).expect("pubkey1 missing");
        let lamports2 = lamports!(received_updates, pubkey2);

        assert_eq!(lamports1.len(), 1);
        assert_eq!(*lamports1.last().unwrap(), 1_000_000);
        assert_eq!(lamports2, None);
    }

    {
        let len = updates_total_len(&received_updates);

        client.subscribe(pubkey2).await.unwrap();
        airdrop(&rpc_client, &pubkey1, 2_000_000).await;
        airdrop(&rpc_client, &pubkey2, 2_000_000).await;

        wait_for_updates(&received_updates, len, 2).await;

        let lamports1 =
            lamports!(received_updates, pubkey1).expect("pubkey1 missing");
        let lamports2 =
            lamports!(received_updates, pubkey2).expect("pubkey2 missing");

        assert_eq!(lamports1.len(), 2);
        assert_eq!(*lamports1.last().unwrap(), 3_000_000);
        assert_eq!(lamports2.len(), 1);
        assert_eq!(*lamports2.last().unwrap(), 3_000_000);
    }

    {
        let len = updates_total_len(&received_updates);

        client.unsubscribe(pubkey1).await.unwrap();
        airdrop(&rpc_client, &pubkey1, 3_000_000).await;
        airdrop(&rpc_client, &pubkey2, 3_000_000).await;

        wait_for_updates(&received_updates, len, 1).await;

        let lamports1 =
            lamports!(received_updates, pubkey1).expect("pubkey1 missing");
        let lamports2 =
            lamports!(received_updates, pubkey2).expect("pubkey2 missing");

        assert_eq!(lamports1.len(), 2);
        assert_eq!(*lamports1.last().unwrap(), 3_000_000);
        assert_eq!(lamports2.len(), 2);
        assert_eq!(*lamports2.last().unwrap(), 6_000_000);
    }
}
