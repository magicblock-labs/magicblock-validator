use log::*;
use std::{sync::Arc, thread};
use test_kit::init_logger;
use tokio::task::JoinSet;

use integration_test_tools::IntegrationTestContext;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer, system_instruction,
};

use crate::utils::init_and_delegate_flexi_counter;
mod utils;

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
        .into_iter()
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

fn spawn_transfer_thread(
    ctx: Arc<IntegrationTestContext>,
    from: Keypair,
    to: Pubkey,
    amount: u64,
) -> thread::JoinHandle<()> {
    let transfer_ix = system_instruction::transfer(&from.pubkey(), &to, amount);
    let from_pk = from.pubkey();
    thread::spawn(move || {
        debug!("Start transfer {amount} {from_pk} -> {to} {{");
        let (sig, confirmed) = ctx
            .send_and_confirm_instructions_with_payer_ephem(
                &[transfer_ix],
                &from,
            )
            .unwrap();
        debug!("Transfer tx: {sig} {confirmed}");
        if confirmed {
            debug!("}} End transfer {amount} {from_pk} -> {to}");
        } else {
            warn!("}} Failed transfer {amount} {from_pk} -> {to}");
        }
        assert!(confirmed);
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_multiple_transfers_from_multiple_escrows_in_parallel() {
    init_logger!();
    let ctx = Arc::new(IntegrationTestContext::try_new().unwrap());

    // 1. Create the account we will transfer to
    debug!("Creating counter account...");
    let kp_counter = Keypair::new();
    let counter_pda = init_and_delegate_flexi_counter(&ctx, &kp_counter);
    // 2. Create 10 escrowed accounts with 2 SOL each
    debug!("Creating 10 escrowed accounts...");
    let escrowed_kps = {
        let escrowed_kps: Vec<Keypair> =
            (0..10).map(|_| Keypair::new()).collect();
        let mut join_set = JoinSet::new();
        for kp_escrowed in escrowed_kps.into_iter() {
            let ctx = ctx.clone();
            join_set.spawn(async move {
                ctx.airdrop_chain_escrowed(&kp_escrowed, 2 * LAMPORTS_PER_SOL)
                    .await
                    .unwrap();
                kp_escrowed
            });
        }
        join_set.join_all().await
    };

    // 3. Get all escrowed accounts to ensure they are cloned _before_ we run
    //    the transfers in parallel
    // NOTE: this step also locks up the validator already
    debug!("Fetching all escrowed accounts to ensure they are cloned...");
    ctx.fetch_ephem_multiple_accounts(
        &escrowed_kps
            .iter()
            .map(|kp| kp.pubkey())
            .collect::<Vec<Pubkey>>(),
    )
    .unwrap();

    // 4. Transfer 0.5 SOL from each escrowed account to counter pda in parallel
    // NOTE: we are using threads here instead of tokio tasks like in the above
    // test that includes cloning in order to guarantee parallelism
    debug!("Transferring 0.5 SOL from each escrowed account to counter pda...");

    let mut handles = vec![];
    let transfer_amount = 1_000_000;
    // acc 1,2,3
    for kp_escrowed in escrowed_kps.iter().take(3) {
        handles.push(spawn_transfer_thread(
            ctx.clone(),
            kp_escrowed.insecure_clone(),
            counter_pda,
            transfer_amount,
        ));
    }
    // acc 4
    handles.push(spawn_transfer_thread(
        ctx.clone(),
        escrowed_kps[3].insecure_clone(),
        counter_pda,
        transfer_amount,
    ));
    // acc 5,6
    for kp_escrowed in escrowed_kps.iter().skip(4).take(2) {
        handles.push(spawn_transfer_thread(
            ctx.clone(),
            kp_escrowed.insecure_clone(),
            counter_pda,
            transfer_amount,
        ));
    }
    // acc 7,8,9
    for kp_escrowed in escrowed_kps.iter().skip(6).take(3) {
        handles.push(spawn_transfer_thread(
            ctx.clone(),
            kp_escrowed.insecure_clone(),
            counter_pda,
            transfer_amount,
        ));
    }
    // acc 10
    handles.push(spawn_transfer_thread(
        ctx.clone(),
        escrowed_kps[9].insecure_clone(),
        counter_pda,
        transfer_amount,
    ));
    debug!("Waiting for transfers to complete...");
    handles.into_iter().for_each(|h| h.join().unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_multiple_transfers_from_same_escrow_different_amounts_in_parallel(
) {
    init_logger!();
    let ctx = Arc::new(IntegrationTestContext::try_new().unwrap());

    // 1. Create the account we will transfer to
    debug!("Creating counter account...");

    let kp_counter = Keypair::new();
    let counter_pda = init_and_delegate_flexi_counter(&ctx, &kp_counter);
    // 2. Create escrowed account
    debug!("Creating escrowed account...");

    let kp_escrowed = Keypair::new();
    ctx.airdrop_chain_escrowed(&kp_escrowed, 20 * LAMPORTS_PER_SOL)
        .await
        .unwrap();

    // 3. Fetch escrowed account to ensure that the fetch + clone already happened before
    //    we send the transfer transactions
    let acc = ctx.fetch_ephem_account(kp_escrowed.pubkey()).unwrap();
    debug!("Fetched {acc:#?}");

    // 4. Run multiple system transfer transactions for the same accounts in parallel
    let mut handles = vec![];
    for i in 0..10 {
        let transfer_amount = LAMPORTS_PER_SOL + i;
        handles.push(spawn_transfer_thread(
            ctx.clone(),
            kp_escrowed.insecure_clone(),
            counter_pda,
            transfer_amount,
        ));
    }
    debug!("Waiting for transfers to complete...");
    handles.into_iter().for_each(|h| h.join().unwrap());
}
