use log::*;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_undelegated,
    assert_loaded_program_with_min_size, assert_loaded_program_with_size,
    assert_not_subscribed, assert_subscribed_without_delegation_record,
    assert_subscribed_without_loaderv3_program_data_account,
    remote_account_provider::program_account::RemoteProgramLoader,
    testing::{init_logger, utils::random_pubkey},
    AccountFetchOrigin,
};
use solana_loader_v4_interface::state::LoaderV4Status;
use solana_pubkey::Pubkey;
use solana_sdk::{signature::Keypair, signer::Signer};
use test_chainlink::{
    accounts::{sanitized_transaction_with_accounts, TransactionAccounts},
    ixtest_context::IxtestContext,
    logging::{stringify_maybe_pubkeys, stringify_pubkeys},
    programs::MEMOV2,
    sleep_ms,
};
use tokio::task;

#[tokio::test]
async fn ixtest_accounts_for_tx_2_delegated_3_readonly_3_programs_one_native() {
    init_logger();

    let ctx = IxtestContext::init().await;

    // 2 Delegated accounts
    let deleg_counter_auth1 = Keypair::new();
    let deleg_counter_auth2 = Keypair::new();
    let mut init_joinset = task::JoinSet::new();
    for counter in [&deleg_counter_auth1, &deleg_counter_auth2] {
        let ctx = ctx.clone();
        let counter = counter.insecure_clone();
        init_joinset.spawn(async move {
            ctx.init_counter(&counter)
                .await
                .delegate_counter(&counter)
                .await;
        });
    }

    let deleg_counter_pda1 = ctx.counter_pda(&deleg_counter_auth1.pubkey());
    let deleg_counter_pda2 = ctx.counter_pda(&deleg_counter_auth2.pubkey());

    // 3 readonly accounts (not delegated)
    let readonly_counter_auth1 = Keypair::new();
    let readonly_counter_auth2 = Keypair::new();
    let readonly_counter_auth3 = Keypair::new();
    for counter in [
        &readonly_counter_auth1,
        &readonly_counter_auth2,
        &readonly_counter_auth3,
    ] {
        let ctx = ctx.clone();
        let counter = counter.insecure_clone();
        init_joinset.spawn(async move {
            ctx.init_counter(&counter).await;
        });
    }

    init_joinset.join_all().await;

    let readonly_counter_pda1 =
        ctx.counter_pda(&readonly_counter_auth1.pubkey());
    let readonly_counter_pda2 =
        ctx.counter_pda(&readonly_counter_auth2.pubkey());
    let readonly_counter_pda3 =
        ctx.counter_pda(&readonly_counter_auth3.pubkey());

    // Programs
    let program_flexi_counter = program_flexi_counter::id();
    let program_system = solana_sdk::system_program::id();

    let tx_accounts = TransactionAccounts {
        readonly_accounts: vec![
            readonly_counter_pda1,
            readonly_counter_pda2,
            readonly_counter_pda3,
        ],
        writable_accounts: vec![deleg_counter_pda1, deleg_counter_pda2],
        programs: vec![program_flexi_counter, program_system, MEMOV2],
    };
    let tx = sanitized_transaction_with_accounts(&tx_accounts);

    let res = ctx
        .chainlink
        .ensure_transaction_accounts(&tx)
        .await
        .unwrap();

    debug!("res: {res:?}");
    debug!("{}", ctx.cloner);

    // Verify cloned accounts
    assert_cloned_as_undelegated!(
        ctx.cloner,
        &[
            readonly_counter_pda1,
            readonly_counter_pda2,
            readonly_counter_pda3,
        ]
    );

    assert_cloned_as_delegated!(
        ctx.cloner,
        &[deleg_counter_pda1, deleg_counter_pda2,]
    );

    // Verify loaded programs
    assert_eq!(ctx.cloner.cloned_programs_count(), 2);
    assert_loaded_program_with_min_size!(
        ctx.cloner,
        &program_flexi_counter,
        &Pubkey::default(),
        RemoteProgramLoader::V3,
        LoaderV4Status::Deployed,
        74800
    );
    assert_loaded_program_with_size!(
        ctx.cloner,
        &MEMOV2,
        &MEMOV2,
        RemoteProgramLoader::V2,
        LoaderV4Status::Finalized,
        74800
    );

    // Verify subscriptions
    assert_subscribed_without_delegation_record!(
        ctx.chainlink,
        &[
            readonly_counter_pda1,
            readonly_counter_pda2,
            readonly_counter_pda3,
        ]
    );
    assert_subscribed_without_loaderv3_program_data_account!(
        ctx.chainlink,
        &[program_flexi_counter, MEMOV2]
    );
    assert_not_subscribed!(
        ctx.chainlink,
        &[deleg_counter_pda1, deleg_counter_pda2, program_system]
    );

    // -----------------
    // Fetch Accounts
    // -----------------
    // We should now get all accounts from the bank without fetching anything
    // An account that does not exist on chain will be returned as None
    let (all_pubkeys, all_pubkeys_strs, new_pubkey) = {
        let mut all_pubkeys = tx_accounts.all_sorted();
        let new_pubkey = random_pubkey();
        all_pubkeys.push(new_pubkey);
        all_pubkeys.sort();
        let all_pubkeys_strs = stringify_pubkeys(&all_pubkeys);
        (all_pubkeys, all_pubkeys_strs, new_pubkey)
    };

    // Initially the new_pubkey does not exist on chain so it will be returned as None
    {
        let (fetched_pubkeys, fetched_strs) = {
            let fetched_accounts = ctx
                .chainlink
                .fetch_accounts(&all_pubkeys, AccountFetchOrigin::GetAccount)
                .await
                .unwrap();
            let mut fetched_pubkeys = all_pubkeys
                .iter()
                .zip(fetched_accounts.iter())
                .map(|(pk, acc)| acc.as_ref().map(|_| *pk))
                .collect::<Vec<Option<Pubkey>>>();
            fetched_pubkeys.sort();
            let fetched_strs = stringify_maybe_pubkeys(&fetched_pubkeys);
            (fetched_pubkeys, fetched_strs)
        };

        let (expected_pubkeys, expected_strs) = {
            let mut expected_pubkeys = all_pubkeys
                .iter()
                .map(|pk| if pk == &new_pubkey { None } else { Some(*pk) })
                .collect::<Vec<Option<Pubkey>>>();
            expected_pubkeys.sort();
            let expected_strs = stringify_maybe_pubkeys(&expected_pubkeys);
            (expected_pubkeys, expected_strs)
        };

        debug!("all_pubkeys: {all_pubkeys_strs:#?} ({})", all_pubkeys.len());
        debug!(
            "fetched_pubkeys: {fetched_strs:#?} ({})",
            fetched_pubkeys.len()
        );
        debug!(
            "expected_pubkeys: {expected_strs:#?} ({})",
            expected_pubkeys.len()
        );
        assert_eq!(fetched_pubkeys, expected_pubkeys);
    }

    // After we add the account to chain and run the same request again it will
    // return all accounts
    {
        ctx.rpc_client
            .request_airdrop(&new_pubkey, 1_000_000_000)
            .await
            .unwrap();

        sleep_ms(500).await;

        let (fetched_pubkeys, fetched_strs) = {
            let fetched_accounts = ctx
                .chainlink
                .fetch_accounts(&all_pubkeys, AccountFetchOrigin::GetAccount)
                .await
                .unwrap();
            let mut fetched_pubkeys = all_pubkeys
                .iter()
                .zip(fetched_accounts.iter())
                .map(|(pk, acc)| acc.as_ref().map(|_| *pk))
                .collect::<Vec<Option<Pubkey>>>();
            fetched_pubkeys.sort();
            let fetched_strs = stringify_maybe_pubkeys(&fetched_pubkeys);
            (fetched_pubkeys, fetched_strs)
        };
        let (expected_pubkeys, expected_strs) = {
            let mut expected_pubkeys = all_pubkeys
                .iter()
                .map(|pk| Option::Some(*pk))
                .collect::<Vec<Option<Pubkey>>>();
            expected_pubkeys.sort();
            let expected_strs = stringify_maybe_pubkeys(&expected_pubkeys);
            (expected_pubkeys, expected_strs)
        };

        debug!(
            "fetched_pubkeys: {fetched_strs:#?} ({})",
            fetched_pubkeys.len()
        );
        debug!(
            "expected_pubkeys: {expected_strs:#?} ({})",
            expected_pubkeys.len()
        );

        assert_eq!(fetched_pubkeys, expected_pubkeys);
    }
}
