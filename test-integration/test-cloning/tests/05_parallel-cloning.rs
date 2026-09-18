use std::{sync::Arc, thread};

use integration_test_tools::{init_logger, IntegrationTestContext};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer,
};
use tracing::*;

fn random_pubkey() -> Pubkey {
    Keypair::new().pubkey()
}

#[test]
fn test_get_multiple_existing_accounts_in_parallel() {
    init_logger!();

    // This test is used to ensure we don't lock up when multiple parallel requests
    // require fetching + cloning one or more accounts
    let [acc1, acc2, acc3, acc4, acc5, acc6, acc7, acc8, acc9, acc10] = [
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
    ];
    let accs = [acc1, acc2, acc3, acc4, acc5, acc6, acc7, acc8, acc9, acc10];
    let ctx = Arc::new(IntegrationTestContext::try_new().unwrap());

    debug!("Airdropping 2 SOL to each of 10 accounts...");
    accs.iter()
        .map(|&acc| {
            let ctx = ctx.clone();
            thread::spawn(move || {
                ctx.airdrop_chain(&acc, 2 * LAMPORTS_PER_SOL)
                    .expect("failed to airdrop to on-chain account");
            })
        })
        .for_each(|h| h.join().unwrap());
    debug!("Airdrops complete.");

    // Create multiple threads to fetch one or more accounts in parallel
    let mut handles = vec![];

    // acc 1,2,3
    handles.push(thread::spawn({
        let ctx = ctx.clone();
        move || {
            debug!("Start thread 1,2,3 {{");
            let fetched = ctx
                .fetch_ephem_multiple_accounts(&[acc1, acc2, acc3])
                .unwrap();
            debug!("}} End thread 1,2,3");
            assert_eq!(fetched.len(), 3);
            assert!(fetched.iter().all(|acc| acc.is_some()));
        }
    }));
    // acc 4
    handles.push(thread::spawn({
        let ctx = ctx.clone();
        move || {
            debug!("Start thread 4 {{");
            let fetched = ctx.fetch_ephem_account(acc4).unwrap();
            debug!("}} End thread 4");
            assert_eq!(fetched.lamports, 2 * LAMPORTS_PER_SOL);
        }
    }));
    // acc 5,6
    handles.push(thread::spawn({
        let ctx = ctx.clone();
        move || {
            debug!("Start thread 5,6 {{");
            let fetched =
                ctx.fetch_ephem_multiple_accounts(&[acc5, acc6]).unwrap();
            debug!("}} End thread 5,6");
            assert_eq!(fetched.len(), 2);
            assert!(fetched.iter().all(|acc| acc.is_some()));
        }
    }));
    // acc 7,8,9
    handles.push(thread::spawn({
        let ctx = ctx.clone();
        move || {
            debug!("Start thread 7,8,9 {{");
            let fetched = ctx
                .fetch_ephem_multiple_accounts(&[acc7, acc8, acc9])
                .unwrap();
            debug!("}} End thread 7,8,9");
            assert_eq!(fetched.len(), 3);
            assert!(fetched.iter().all(|acc| acc.is_some()));
        }
    }));
    // acc 10
    handles.push(thread::spawn({
        let ctx = ctx.clone();
        move || {
            debug!("Start thread 10 {{");
            let fetched = ctx.fetch_ephem_account(acc10).unwrap();
            debug!("}} End thread 10");
            assert_eq!(fetched.lamports, 2 * LAMPORTS_PER_SOL);
        }
    }));

    debug!("Waiting for threads to complete...");
    handles.into_iter().for_each(|h| h.join().unwrap());
}
