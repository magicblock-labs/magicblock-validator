use dlp_api::{
    args::{
        EncryptedBuffer, MaybeEncryptedInstruction, MaybeEncryptedIxData,
        PostDelegationActions,
    },
    pda::delegation_record_pda_from_delegated_account,
    state::DelegationRecord,
};
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_not_subscribed,
    assert_remain_undelegating, assert_subscribed_without_delegation_record,
    testing::{context::TestContext, deleg::delegation_record_to_vec},
};
use solana_account::{Account, ReadableAccount};
use solana_pubkey::Pubkey;

const INITIAL_SLOT: u64 = 22;

/// Publishes the delegation generation separately from its RPC observation slot.
fn add_record(
    ctx: &TestContext,
    pubkey: Pubkey,
    owner: Pubkey,
    slot: u64,
    actions: Option<PostDelegationActions>,
) -> Pubkey {
    let record = DelegationRecord {
        authority: ctx.validator_pubkey,
        owner,
        delegation_slot: slot,
        lamports: 1_000,
        commit_frequency_ms: 2_000,
    };
    let mut data = delegation_record_to_vec(&record);
    if let Some(actions) = actions {
        data.extend_from_slice(&borsh::to_vec(&actions).unwrap());
    }
    let record_pubkey = delegation_record_pda_from_delegated_account(&pubkey);
    ctx.rpc_client.add_account(
        record_pubkey,
        Account {
            owner: dlp_api::id(),
            data,
            ..Default::default()
        },
    );
    record_pubkey
}

fn failing_action() -> PostDelegationActions {
    // Invalid v42 discriminator: activation must not leave a usable delegation.
    PostDelegationActions {
        inserted_signers: 0,
        inserted_non_signers: 0,
        signers: vec![*v42_calculator_interface::ID.as_array()],
        non_signers: vec![],
        instructions: vec![MaybeEncryptedInstruction {
            program_id: 0,
            accounts: vec![],
            data: MaybeEncryptedIxData {
                prefix: vec![0xFF],
                suffix: EncryptedBuffer::default(),
            },
        }],
    }
}

async fn undelegating_account() -> (TestContext, Pubkey, Pubkey, Account) {
    let ctx = TestContext::init(INITIAL_SLOT).await;
    let pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let remote = Account {
        lamports: 1_000_000,
        owner: dlp_api::id(),
        data: vec![1],
        ..Default::default()
    };
    ctx.rpc_client.add_account(pubkey, remote.clone());
    let record = add_record(&ctx, pubkey, owner, INITIAL_SLOT, None);
    ctx.ensure_account(&pubkey).await.unwrap();
    assert_cloned_as_delegated!(ctx.bank, &[pubkey], INITIAL_SLOT, owner);
    assert_not_subscribed!(ctx.chainlink, &[&pubkey, &record]);
    ctx.force_undelegation(&pubkey).await;
    ctx.chainlink.undelegation_requested(pubkey).await.unwrap();
    assert_remain_undelegating!(ctx.bank, &[pubkey], INITIAL_SLOT);
    (ctx, pubkey, owner, remote)
}

/// Proves an atomic base-chain callback can redelegate at a newer slot without
/// any intermediate ReadOnly notification, restoring the complete account image.
#[tokio::test]
async fn redelegation_without_readonly_notification() {
    let (ctx, pubkey, owner, mut remote) = undelegating_account().await;
    let slot = ctx.rpc_client.set_slot(INITIAL_SLOT + 1);
    let record = add_record(&ctx, pubkey, owner, slot, None);
    remote.lamports += 123;
    remote.data = vec![2, 3, 4];
    assert!(
        ctx.send_and_receive_account_update(
            pubkey,
            remote.clone(),
            Some(8_000)
        )
        .await
    );
    assert_cloned_as_delegated!(ctx.bank, &[pubkey], slot, owner);
    ctx.bank
        .accounts()
        .loader()
        .read(&pubkey, |local| {
            assert_eq!(local.lamports(), remote.lamports);
            assert_eq!(local.data(), remote.data);
        })
        .unwrap()
        .expect("local account");
    assert_not_subscribed!(ctx.chainlink, &[&pubkey, &record]);
}

/// Proves a newer notification does not turn an old delegation into a new generation.
#[tokio::test]
async fn old_delegation_keeps_recovery_subscription() {
    let (ctx, pubkey, owner, remote) = undelegating_account().await;
    ctx.rpc_client.set_slot(INITIAL_SLOT + 1);
    add_record(&ctx, pubkey, owner, INITIAL_SLOT, None);
    assert!(
        ctx.send_and_receive_account_update(pubkey, remote, Some(8_000))
            .await
    );
    assert_remain_undelegating!(ctx.bank, &[pubkey], INITIAL_SLOT);
    assert_subscribed_without_delegation_record!(ctx.chainlink, &[&pubkey]);
}

/// Proves failed activation retains recovery subscriptions and a later valid
/// delegation can restore the account without an intermediate ReadOnly update.
#[tokio::test]
async fn failed_redelegation_can_recover() {
    let (ctx, pubkey, owner, remote) = undelegating_account().await;
    let slot = ctx.rpc_client.set_slot(INITIAL_SLOT + 1);
    let record = add_record(&ctx, pubkey, owner, slot, Some(failing_action()));
    assert!(
        ctx.send_and_receive_account_update(
            pubkey,
            remote.clone(),
            Some(8_000)
        )
        .await
    );
    assert_remain_undelegating!(ctx.bank, &[pubkey], INITIAL_SLOT);
    assert_subscribed_without_delegation_record!(ctx.chainlink, &[&pubkey]);

    let slot = ctx.rpc_client.set_slot(slot + 1);
    add_record(&ctx, pubkey, owner, slot, None);
    assert!(
        ctx.send_and_receive_account_update(pubkey, remote, Some(8_000))
            .await
    );
    assert_cloned_as_delegated!(ctx.bank, &[pubkey], slot, owner);
    assert_not_subscribed!(ctx.chainlink, &[&pubkey, &record]);
}
