use dlp_api::{
    args::{
        EncryptedBuffer, MaybeEncryptedAccountMeta, MaybeEncryptedInstruction,
        MaybeEncryptedIxData, PostDelegationActions,
    },
    pda::delegation_record_pda_from_delegated_account,
    state::DelegationRecord,
};
use magicblock_chainlink::testing::{context::TestContext, init_logger};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;

const INVALID_V42_INSTRUCTION: u8 = 0xFF;

/// Adds a delegation record (delegated to our validator) whose post-delegation
/// action is a v42 instruction carrying an invalid discriminator.
///
/// The action's only account is the delegated target itself, so cloning the
/// target does not require fetching any extra dependency first.
fn add_delegation_record_with_failing_action(
    ctx: &TestContext,
    delegated_pubkey: Pubkey,
    owner: Pubkey,
) {
    let record = DelegationRecord {
        authority: ctx.validator_pubkey,
        owner,
        delegation_slot: 1,
        lamports: 1_000,
        commit_frequency_ms: 2_000,
    };
    let mut data = vec![0; DelegationRecord::size_with_discriminator()];
    record.to_bytes_with_discriminator(&mut data).unwrap();

    let actions = PostDelegationActions {
        inserted_signers: 0,
        inserted_non_signers: 0,
        // index 0 -> the delegated target, index 1 -> v42 program
        signers: vec![
            *delegated_pubkey.as_array(),
            *v42_calculator_interface::ID.as_array(),
        ],
        non_signers: vec![],
        instructions: vec![MaybeEncryptedInstruction {
            program_id: 1,
            accounts: vec![MaybeEncryptedAccountMeta::ClearText(
                // account index 0 -> the delegated target, as signer
                dlp_api::compact::AccountMeta::new_readonly(0, true),
            )],
            data: MaybeEncryptedIxData {
                prefix: vec![INVALID_V42_INSTRUCTION],
                suffix: EncryptedBuffer::default(),
            },
        }],
    };
    data.extend_from_slice(&borsh::to_vec(&actions).unwrap());

    ctx.rpc_client.add_account(
        delegation_record_pda_from_delegated_account(&delegated_pubkey),
        Account {
            owner: dlp_api::id(),
            data,
            ..Default::default()
        },
    );
}

/// A delegated account whose post-delegation action cannot be executed (here a
/// v42 instruction with an invalid discriminator) must not be materialized as
/// a usable delegated account.
#[tokio::test]
async fn failing_post_delegation_action_is_rejected() {
    init_logger();
    let ctx = TestContext::init(100).await;

    let delegated_pubkey = Pubkey::new_unique();
    let owner = system_program::id();

    // Account on chain, owned by the delegation program (delegated to us).
    ctx.rpc_client.add_account(
        delegated_pubkey,
        Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3, 4],
            owner: dlp_api::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    add_delegation_record_with_failing_action(&ctx, delegated_pubkey, owner);

    let err = ctx
        .ensure_account(&delegated_pubkey)
        .await
        .expect_err("invalid post-delegation action must not be accepted");
    assert!(
        err.to_string().contains("Failed to clone"),
        "invalid action must fail account materialization: {err}"
    );
}
