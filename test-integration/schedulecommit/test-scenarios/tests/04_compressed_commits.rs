#![allow(clippy::result_large_err)]

use borsh::BorshDeserialize;
use compressed_delegation_client::CompressedDelegationRecord;
use integration_test_tools::{
    loaded_accounts::DLP_TEST_AUTHORITY_BYTES,
    scheduled_commits::{
        extract_scheduled_commit_sent_signature_from_logs,
        extract_sent_commit_info_from_logs,
    },
    IntegrationTestContext,
};
use light_client::indexer::{
    photon_indexer::PhotonIndexer, AddressWithTree, Indexer,
    ValidityProofWithContext,
};
use light_sdk::{
    instruction::{
        account_meta::CompressedAccountMeta, PackedAccounts,
        SystemAccountMetaConfig,
    },
    sdk_types::PackedAddressTreeInfo,
};
use magicblock_core::compression::{
    derive_cda_from_pda, ADDRESS_TREE, OUTPUT_QUEUE,
};
use program_flexi_counter::{
    instruction::{
        create_delegate_compressed_ix,
        create_init_compressed_delegation_record_ix, create_init_ix,
        create_schedule_commit_compressed_ix, DelegateCompressedArgs,
        InitDelegationRecordArgs,
    },
    state::FlexiCounter,
};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

#[tokio::test(flavor = "multi_thread")]
async fn test_schedule_compressed_commit() {
    test_kit::init_logger!();

    let ctx = IntegrationTestContext::try_new().unwrap();
    let chain_client = ctx.chain_client.as_ref().unwrap();
    let ephem_client = ctx.ephem_client.as_ref().unwrap();
    let photon_indexer = ctx.photon_client.as_ref().unwrap();

    let payer_chain = Keypair::new();
    let counter_auth = Keypair::new();
    let validator_identity = ctx.ephem_validator_identity.unwrap();

    ctx.airdrop_chain(&payer_chain.pubkey(), LAMPORTS_PER_SOL * 10)
        .unwrap();
    ctx.airdrop_chain(&counter_auth.pubkey(), LAMPORTS_PER_SOL * 10)
        .unwrap();

    let validator_keypair =
        Keypair::try_from(&DLP_TEST_AUTHORITY_BYTES[..]).unwrap();
    ctx.ensure_magic_fee_vault_delegated_on_chain(&validator_keypair)
        .unwrap();

    let (counter_pda, compressed_record_address) =
        init_and_delegate_compressed_counter(
            &chain_client,
            &photon_indexer,
            &counter_auth,
            validator_identity,
        )
        .await;

    let (_deleg_sig, confirmed) =
        ctx.delegate_account(&payer_chain, &counter_auth).unwrap();
    assert!(confirmed, "failed to delegate ephemeral fee payer");

    let schedule_sig = ephem_client
        .send_and_confirm_transaction(&mut Transaction::new_signed_with_payer(
            &[create_schedule_commit_compressed_ix(
                &counter_auth.pubkey(),
                &validator_identity,
            )],
            Some(&counter_auth.pubkey()),
            &[&counter_auth],
            ephem_client.get_latest_blockhash().unwrap(),
        ))
        .unwrap();

    let schedule_logs = ctx.fetch_ephemeral_logs(schedule_sig).unwrap();
    let scheduled_commit_sent_sig =
        extract_scheduled_commit_sent_signature_from_logs(&schedule_logs)
            .unwrap();
    let sent_logs =
        ctx.fetch_ephemeral_logs(scheduled_commit_sent_sig).unwrap();
    let (included, excluded, _feepayers, chain_sigs) =
        extract_sent_commit_info_from_logs(&sent_logs);
    for sig in &chain_sigs {
        assert!(
            ctx.confirm_transaction_chain(sig, None).unwrap(),
            "chain commit transaction {sig} should confirm"
        );
    }

    assert!(
        included.contains(&counter_pda),
        "compressed counter should be included in scheduled commit result: {included:#?}"
    );
    assert!(
        excluded.is_empty(),
        "compressed counter should not be excluded from scheduled commit result: {excluded:#?}"
    );

    let compressed_account = photon_indexer
        .get_compressed_account(compressed_record_address, None)
        .await
        .unwrap()
        .value
        .unwrap();
    let compressed_record = CompressedDelegationRecord::try_from_slice(
        &compressed_account.data.unwrap().data,
    )
    .unwrap();

    let committed_counter =
        FlexiCounter::try_from_slice(&compressed_record.data).unwrap();
    assert_eq!(
        committed_counter.count, 0,
        "compressed delegation record should contain committed counter state"
    );
}

async fn init_and_delegate_compressed_counter(
    chain_client: &RpcClient,
    photon_indexer: &PhotonIndexer,
    counter_auth: &Keypair,
    validator_identity: solana_sdk::pubkey::Pubkey,
) -> (solana_sdk::pubkey::Pubkey, [u8; 32]) {
    let (counter_pda, _) = FlexiCounter::pda(&counter_auth.pubkey());
    let compressed_record_address =
        derive_cda_from_pda(&counter_pda).to_bytes();

    let init_counter_ix =
        create_init_ix(counter_auth.pubkey(), "COUNTER".to_string());
    let init_record_ix = create_init_compressed_record_ix(
        counter_auth.pubkey(),
        compressed_record_address,
        photon_indexer,
    )
    .await;

    send_chain_ixs(
        &chain_client,
        counter_auth,
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            init_counter_ix,
            init_record_ix,
        ],
    )
    .await;

    let delegate_ix = create_delegate_compressed_counter_ix(
        counter_auth.pubkey(),
        compressed_record_address,
        &photon_indexer,
        validator_identity,
    )
    .await;
    send_chain_ixs(
        &chain_client,
        counter_auth,
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            delegate_ix,
        ],
    )
    .await;

    (counter_pda, compressed_record_address)
}

async fn create_init_compressed_record_ix(
    payer: solana_sdk::pubkey::Pubkey,
    compressed_record_address: [u8; 32],
    photon_indexer: &PhotonIndexer,
) -> solana_sdk::instruction::Instruction {
    let mut remaining_accounts = packed_accounts();
    let rpc_result: ValidityProofWithContext = photon_indexer
        .get_validity_proof(
            vec![],
            vec![AddressWithTree {
                address: compressed_record_address,
                tree: ADDRESS_TREE.to_bytes().into(),
            }],
            None,
        )
        .await
        .unwrap()
        .value;

    let address_merkle_tree_pubkey_index =
        remaining_accounts.insert_or_get(ADDRESS_TREE.to_bytes().into());
    let state_queue_pubkey_index =
        remaining_accounts.insert_or_get(OUTPUT_QUEUE.to_bytes().into());
    let (remaining_accounts_metas, _, _) =
        remaining_accounts.to_account_metas();

    create_init_compressed_delegation_record_ix(
        payer,
        &remaining_accounts_metas
            .iter()
            .map(|meta| solana_sdk::message::AccountMeta {
                pubkey: meta.pubkey.to_bytes().into(),
                is_signer: meta.is_signer,
                is_writable: meta.is_writable,
            })
            .collect::<Vec<_>>(),
        InitDelegationRecordArgs {
            validity_proof: rpc_result.proof.into(),
            address_tree_info: PackedAddressTreeInfo {
                root_index: rpc_result.addresses[0].root_index,
                address_merkle_tree_pubkey_index,
                address_queue_pubkey_index: address_merkle_tree_pubkey_index,
            }
            .into(),
            output_state_tree_index: state_queue_pubkey_index,
        },
    )
}

async fn create_delegate_compressed_counter_ix(
    payer: solana_sdk::pubkey::Pubkey,
    compressed_record_address: [u8; 32],
    photon_indexer: &PhotonIndexer,
    validator_identity: solana_sdk::pubkey::Pubkey,
) -> solana_sdk::instruction::Instruction {
    let compressed_account = photon_indexer
        .get_compressed_account(compressed_record_address, None)
        .await
        .unwrap()
        .value
        .unwrap();
    let mut remaining_accounts = packed_accounts();
    let rpc_result: ValidityProofWithContext = photon_indexer
        .get_validity_proof(vec![compressed_account.hash], vec![], None)
        .await
        .unwrap()
        .value;

    let state_queue_pubkey_index =
        remaining_accounts.insert_or_get(OUTPUT_QUEUE.to_bytes().into());
    let packed_trees_info = rpc_result.pack_tree_infos(&mut remaining_accounts);
    let packed_state_tree_info = packed_trees_info.state_trees.unwrap();
    let (remaining_accounts_metas, _, _) =
        remaining_accounts.to_account_metas();

    create_delegate_compressed_ix(
        payer,
        &remaining_accounts_metas
            .iter()
            .map(|meta| solana_sdk::message::AccountMeta {
                pubkey: meta.pubkey.to_bytes().into(),
                is_signer: meta.is_signer,
                is_writable: meta.is_writable,
            })
            .collect::<Vec<_>>(),
        DelegateCompressedArgs {
            validator: Some(validator_identity),
            validity_proof: rpc_result.proof.into(),
            account_meta: CompressedAccountMeta {
                tree_info: packed_state_tree_info.packed_tree_infos[0],
                address: compressed_account.address.unwrap(),
                output_state_tree_index: state_queue_pubkey_index,
            }
            .into(),
        },
    )
}

fn packed_accounts() -> PackedAccounts {
    let mut remaining_accounts = PackedAccounts::default();
    remaining_accounts
        .add_system_accounts_v2(SystemAccountMetaConfig::new(
            compressed_delegation_client::ID.to_bytes().into(),
        ))
        .unwrap();
    remaining_accounts
}

async fn send_chain_ixs(
    rpc_client: &RpcClient,
    payer: &Keypair,
    ixs: &[solana_sdk::instruction::Instruction],
) {
    let latest_blockhash = rpc_client.get_latest_blockhash().unwrap();
    rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &Transaction::new_signed_with_payer(
                ixs,
                Some(&payer.pubkey()),
                &[payer],
                latest_blockhash,
            ),
            rpc_client.commitment(),
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .unwrap();
}
