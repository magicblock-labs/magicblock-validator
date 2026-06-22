use dlp_api::{
    args::{
        EncryptedBuffer, MaybeEncryptedAccountMeta, MaybeEncryptedInstruction,
        MaybeEncryptedIxData, PostDelegationActions,
    },
    pda::delegation_record_pda_from_delegated_account,
    state::DelegationRecord,
};
use magicblock_chainlink::testing::init_logger;
use solana_account::{Account, AccountSharedData};
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use test_chainlink::test_context::TestContext;

/// A discriminator the flexi-counter program does not recognize. Executing an
/// instruction with this prefix on chain fails with `InvalidInstructionData`.
const INVALID_FLEXI_COUNTER_DISCRIMINATOR: u8 = 0xFF;

/// Adds a delegation record (delegated to our validator) whose post-delegation
/// action is a flexi-counter instruction carrying an invalid discriminator.
///
/// The action's only account is the delegated target itself, so cloning the
/// target does not require fetching any extra dependency first.
fn add_delegation_record_with_failing_flexi_counter_action(
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
        // index 0 -> the delegated target, index 1 -> flexi-counter program
        signers: vec![
            *delegated_pubkey.as_array(),
            *program_flexi_counter::id().as_array(),
        ],
        non_signers: vec![],
        instructions: vec![MaybeEncryptedInstruction {
            // program_id index 1 -> flexi-counter program
            program_id: 1,
            accounts: vec![MaybeEncryptedAccountMeta::ClearText(
                // account index 0 -> the delegated target, as signer
                dlp_api::compact::AccountMeta::new_readonly(0, true),
            )],
            data: MaybeEncryptedIxData {
                prefix: vec![INVALID_FLEXI_COUNTER_DISCRIMINATOR],
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
/// flexi-counter instruction with an invalid discriminator) must not be left
/// delegated in the ER: it is still cloned, but flagged for automatic
/// undelegation back to chain.
#[tokio::test]
async fn post_delegation_failing_flexi_counter_action_schedules_undelegation() {
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
    add_delegation_record_with_failing_flexi_counter_action(
        &ctx,
        delegated_pubkey,
        owner,
    );

    // Make the flexi-counter program present in the bank so the action's
    // program dependency is not fetched (and cloned) before the target,
    // keeping the simulated failure scoped to the target clone below.
    ctx.bank
        .insert(program_flexi_counter::id(), AccountSharedData::default());

    // The cloner stub does not execute instructions, so simulate the on-chain
    // rejection of the invalid discriminator: the clone that would run the
    // post-delegation action fails.
    ctx.cloner.set_fail_next_clone(true);

    ctx.ensure_account(&delegated_pubkey).await.expect(
        "account with a failing post-delegation action should still be cloned",
    );

    assert_eq!(
        ctx.cloner.undelegation_requests(),
        vec![delegated_pubkey],
        "a failing post-delegation action must schedule undelegation"
    );
    // Account is cloned because we are using the stub cloner, which does not execute instructions.
    assert!(
        ctx.cloner.get_account(&delegated_pubkey).is_some(),
        "the account must still be cloned despite the failing action"
    );
}
