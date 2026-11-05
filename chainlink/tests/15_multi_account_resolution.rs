#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use delegation_program_interface as dpi;
use engine::IntoTransactionView;
use magicblock_chainlink::{
    AccountFetchEntrypoint, assert_cloned_as_delegated, assert_cloned_as_undelegated,
    assert_not_subscribed, assert_subscribed,
    testing::{context::TestContext, deleg::add_delegation_record_for, init_logger},
};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use v42_calculator_interface::ID as V42_ID;

#[tokio::test]
async fn resolves_mixed_transaction_accounts_in_one_request() {
    init_logger();
    let ctx = TestContext::init(100).await;
    let payer = Keypair::new();
    let writable = Pubkey::new_unique();
    let readonly = Pubkey::new_unique();
    let owner = Pubkey::new_unique();

    ctx.rpc_client.add_account(
        payer.pubkey(),
        Account {
            lamports: 1_000_000,
            owner,
            ..Default::default()
        },
    );
    ctx.rpc_client.add_account(
        writable,
        Account {
            lamports: 1_000_000,
            owner: dpi::id(),
            ..Default::default()
        },
    );
    add_delegation_record_for(&ctx.rpc_client, writable, ctx.validator_pubkey, owner);
    ctx.rpc_client.add_account(
        readonly,
        Account {
            lamports: 1_000_000,
            owner,
            ..Default::default()
        },
    );

    let instruction = Instruction::new_with_bytes(
        V42_ID,
        &[],
        vec![AccountMeta::new(writable, false), AccountMeta::new_readonly(readonly, false)],
    );
    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&payer.pubkey()),
        &[&payer],
        ctx.bank.blockhash(),
    )
    .compose(&ctx.bank)
    .unwrap();

    ctx.chainlink
        .ensure_accounts(
            transaction.static_account_keys(),
            AccountFetchEntrypoint::SendTransaction(transaction.signatures()[0]),
        )
        .await
        .unwrap();

    assert_cloned_as_undelegated!(ctx.bank, &[payer.pubkey(), readonly]);
    assert_cloned_as_delegated!(ctx.bank, &[writable]);
    assert_subscribed!(ctx.chainlink, &[&payer.pubkey(), &readonly]);
    assert_not_subscribed!(ctx.chainlink, &[&writable, &V42_ID]);
}

#[tokio::test]
async fn resolves_mixed_dlp_records_across_rpc_batch_boundary() {
    use solana_account::{AccountMode, ReadableAccount};
    use wheels::layout::Encodable;

    const SLOT: u64 = 100;
    let ctx = TestContext::init(SLOT).await;
    let cluster_id = Pubkey::new_unique();
    let (cluster_key, bump) = Pubkey::find_program_address(
        &[dpi::DELEGATION_CLUSTER_SEED, cluster_id.as_ref()],
        &dpi::id(),
    );
    let cluster = dpi::DelegationCluster {
        discriminator: dpi::DelegationCluster::DISCRIMINATOR,
        bump,
        cluster_id,
        current_validator: ctx.validator_pubkey,
        config_authority: Pubkey::new_unique(),
        validator_rotation_authority: Pubkey::new_unique(),
        paused: false,
        protocol_fee_vault: Pubkey::new_unique(),
        challenge_config: None,
    };
    ctx.rpc_client.add_account(
        cluster_key,
        Account {
            owner: dpi::id(),
            data: cluster.encode().unwrap(),
            lamports: 1_000_000,
            ..Default::default()
        },
    );

    let mut keys = Vec::new();
    let mut expected = Vec::new();
    // 34 primaries require two RPC snapshots at three keys per delegation.
    for i in 0..34 {
        let key = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let lamports = 1_000_000 + i;
        ctx.rpc_client.add_account(
            key,
            Account {
                owner: dpi::id(),
                lamports,
                ..Default::default()
            },
        );
        let seq = if i % 2 == 0 {
            add_delegation_record_for(&ctx.rpc_client, key, ctx.validator_pubkey, owner);
            0
        } else {
            let (state_key, bump) = Pubkey::find_program_address(
                &[dpi::DELEGATION_STATE_SEED, key.as_ref()],
                &dpi::id(),
            );
            let state = dpi::DelegationState {
                discriminator: dpi::DelegationState::DISCRIMINATOR,
                bump,
                delegated_account: key,
                delegation_cluster: cluster_key,
                original_owner: owner,
                rent_payer: ctx.validator_pubkey,
                delegation_slot: SLOT,
                lamports,
                last_commit_seq: i as u32,
                undelegation_requester: 0,
                delegated_account_bump: 0,
                seed_count: 0,
                encoded_seeds: vec![],
            };
            ctx.rpc_client.add_account(
                state_key,
                Account {
                    owner: dpi::id(),
                    data: state.encode().unwrap(),
                    lamports: 1_000_000,
                    ..Default::default()
                },
            );
            state.last_commit_seq
        };
        keys.push(key);
        expected.push((owner, lamports, seq));
    }
    ctx.chainlink
        .ensure_accounts(&keys, AccountFetchEntrypoint::RpcGetMultipleAccounts)
        .await
        .unwrap();
    for (key, (owner, lamports, seq)) in keys.iter().zip(expected) {
        let image = ctx.bank.accounts().loader().read(key, |a| {
            (*a.owner(), a.lamports(), a.commit_seq(), a.mode())
        });
        assert_eq!(
            image.unwrap(),
            Some((owner, lamports, seq, AccountMode::Delegated))
        );
    }
}
