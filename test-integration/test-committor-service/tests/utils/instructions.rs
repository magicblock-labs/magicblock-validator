use std::sync::Arc;

use compressed_delegation_client::PackedAddressTreeInfo;
use light_client::indexer::{
    photon_indexer::PhotonIndexer, AddressWithTree, CompressedAccount, Indexer,
    ValidityProofWithContext,
};
use light_sdk::instruction::{PackedAccounts, SystemAccountMetaConfig};
use magicblock_core::compression::derive_cda_from_pda;
use magicblock_program::validator::validator_authority_id;
use program_flexi_counter::{
    instruction::{
        create_delegate_compressed_ix, create_init_ix, DelegateCompressedArgs,
    },
    state::FlexiCounter,
};
use solana_pubkey::Pubkey;
use solana_sdk::{instruction::Instruction, pubkey, rent::Rent};

pub fn init_validator_fees_vault_ix(validator_auth: Pubkey) -> Instruction {
    dlp::instruction_builder::init_validator_fees_vault(
        validator_auth,
        validator_auth,
        validator_auth,
    )
}

const ADDRESS_TREE_PUBKEY: Pubkey =
    pubkey!("amt2kaJA14v3urZbZvnc5v2np8jqvc4Z8zDep5wbtzx");
const OUTPUT_QUEUE_PUBKEY: Pubkey =
    pubkey!("oq1na8gojfdUhsfCpyjNt6h4JaDWtHf1yQj4koBWfto");

pub struct InitAccountAndDelegateIxs {
    pub init: Instruction,
    pub reallocs: Vec<Instruction>,
    pub delegate: Instruction,
    pub pda: Pubkey,
    pub rent_exempt: u64,
}

pub struct InitAccountAndDelegateCompressedIxs {
    pub init: Instruction,
    pub delegate: Instruction,
    pub pda: Pubkey,
    pub address: [u8; 32],
}

pub fn init_account_and_delegate_ixs(
    payer: Pubkey,
    bytes: u64,
) -> InitAccountAndDelegateIxs {
    const MAX_ALLOC: u64 = magicblock_committor_program::consts::MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE as u64;

    use program_flexi_counter::{instruction::*, state::*};

    let init_counter_ix = create_init_ix(payer, "COUNTER".to_string());
    let rent_exempt = Rent::default().minimum_balance(bytes as usize);

    let num_reallocs = bytes.div_ceil(MAX_ALLOC);
    let realloc_ixs = if num_reallocs == 0 {
        vec![]
    } else {
        (0..num_reallocs)
            .map(|i| create_realloc_ix(payer, bytes, i as u16))
            .collect()
    };

    let delegate_ix = create_delegate_ix(payer);
    let pda = FlexiCounter::pda(&payer).0;
    InitAccountAndDelegateIxs {
        init: init_counter_ix,
        reallocs: realloc_ixs,
        delegate: delegate_ix,
        pda,
        rent_exempt,
    }
}

pub async fn init_account_and_delegate_compressed_ixs(
    payer: Pubkey,
    photon_indexer: Arc<PhotonIndexer>,
) -> InitAccountAndDelegateCompressedIxs {
    let (pda, _bump) = FlexiCounter::pda(&payer);
    let record_address = derive_cda_from_pda(&pda);

    let init_counter_ix = create_init_ix(payer, "COUNTER".to_string());

    let system_account_meta_config =
        SystemAccountMetaConfig::new(compressed_delegation_client::ID);
    let mut remaining_accounts = PackedAccounts::default();
    remaining_accounts
        .add_system_accounts_v2(system_account_meta_config)
        .unwrap();

    let (
        remaining_accounts_metas,
        validity_proof,
        address_tree_info,
        account_meta,
        output_state_tree_index,
    ) = {
        let rpc_result: ValidityProofWithContext = photon_indexer
            .get_validity_proof(
                vec![],
                vec![AddressWithTree {
                    address: record_address.to_bytes(),
                    tree: ADDRESS_TREE_PUBKEY,
                }],
                None,
            )
            .await
            .unwrap()
            .value;

        // Insert trees in accounts
        let address_merkle_tree_pubkey_index =
            remaining_accounts.insert_or_get(ADDRESS_TREE_PUBKEY);
        let state_queue_pubkey_index =
            remaining_accounts.insert_or_get(OUTPUT_QUEUE_PUBKEY);

        let packed_address_tree_info = PackedAddressTreeInfo {
            root_index: rpc_result.addresses[0].root_index,
            address_merkle_tree_pubkey_index,
            address_queue_pubkey_index: address_merkle_tree_pubkey_index,
        };

        let (remaining_accounts_metas, _, _) =
            remaining_accounts.to_account_metas();

        (
            remaining_accounts_metas,
            rpc_result.proof,
            Some(packed_address_tree_info),
            None,
            state_queue_pubkey_index,
        )
    };

    let delegate_ix = create_delegate_compressed_ix(
        payer,
        &remaining_accounts_metas,
        DelegateCompressedArgs {
            valid_until: i64::MAX,
            commit_frequency_ms: u32::MAX,
            validator: Some(validator_authority_id()),
            validity_proof,
            account_meta,
            address_tree_info,
            output_state_tree_index,
        },
    );

    InitAccountAndDelegateCompressedIxs {
        init: init_counter_ix,
        delegate: delegate_ix,
        pda,
        address: record_address.to_bytes(),
    }
}
