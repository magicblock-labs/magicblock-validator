#[cfg(any(test, feature = "dev-context"))]
use dlp_api::dlp::args::{
    EncryptedBuffer, MaybeEncryptedInstruction, MaybeEncryptedIxData,
    PostDelegationActions,
};
#[cfg(any(test, feature = "dev-context"))]
use dlp_api::dlp::pda::delegation_record_pda_from_delegated_account;
#[cfg(any(test, feature = "dev-context"))]
use dlp_api::dlp::state::DelegationRecord;
#[cfg(any(test, feature = "dev-context"))]
use solana_account::Account;
#[cfg(any(test, feature = "dev-context"))]
use solana_pubkey::Pubkey;

#[cfg(any(test, feature = "dev-context"))]
use crate::testing::rpc_client_mock::ChainRpcClientMock;

#[cfg(any(test, feature = "dev-context"))]
pub fn delegation_record_to_vec(deleg_record: &DelegationRecord) -> Vec<u8> {
    let size = DelegationRecord::size_with_discriminator();
    let mut data = vec![0; size];
    deleg_record.to_bytes_with_discriminator(&mut data).unwrap();
    data
}

#[cfg(any(test, feature = "dev-context"))]
pub fn add_delegation_record_for(
    rpc_client: &ChainRpcClientMock,
    pubkey: Pubkey,
    authority: Pubkey,
    owner: Pubkey,
) -> Pubkey {
    let deleg_record_pubkey =
        delegation_record_pda_from_delegated_account(&pubkey);
    let deleg_record = DelegationRecord {
        authority,
        owner,
        delegation_slot: 1,
        lamports: 1_000,
        commit_frequency_ms: 2_000,
    };
    rpc_client.add_account(
        deleg_record_pubkey,
        Account {
            owner: dlp_api::dlp::id(),
            data: delegation_record_to_vec(&deleg_record),
            ..Default::default()
        },
    );
    deleg_record_pubkey
}

#[cfg(any(test, feature = "dev-context"))]
pub fn add_delegation_record_with_actions_for(
    rpc_client: &ChainRpcClientMock,
    pubkey: Pubkey,
    authority: Pubkey,
    owner: Pubkey,
    program_id: Pubkey,
) -> Pubkey {
    let deleg_record_pubkey =
        delegation_record_pda_from_delegated_account(&pubkey);
    let deleg_record = DelegationRecord {
        authority,
        owner,
        delegation_slot: 1,
        lamports: 1_000,
        commit_frequency_ms: 2_000,
    };
    let mut data = delegation_record_to_vec(&deleg_record);
    let actions = PostDelegationActions {
        inserted_signers: 0,
        inserted_non_signers: 0,
        signers: vec![*program_id.as_array()],
        non_signers: vec![],
        instructions: vec![MaybeEncryptedInstruction {
            program_id: 0,
            accounts: vec![],
            data: MaybeEncryptedIxData {
                prefix: vec![1],
                suffix: EncryptedBuffer::default(),
            },
        }],
    };
    data.extend_from_slice(&borsh::to_vec(&actions).unwrap());
    rpc_client.add_account(
        deleg_record_pubkey,
        Account {
            owner: dlp_api::dlp::id(),
            data,
            ..Default::default()
        },
    );
    deleg_record_pubkey
}

#[cfg(any(test, feature = "dev-context"))]
pub fn add_invalid_delegation_record_for(
    rpc_client: &ChainRpcClientMock,
    pubkey: Pubkey,
) -> Pubkey {
    let deleg_record_pubkey =
        delegation_record_pda_from_delegated_account(&pubkey);
    // Create invalid delegation record data (corrupted/invalid bytes)
    let invalid_data = vec![255, 255, 255, 255]; // Invalid data
    rpc_client.add_account(
        deleg_record_pubkey,
        Account {
            owner: dlp_api::dlp::id(),
            data: invalid_data,
            ..Default::default()
        },
    );
    deleg_record_pubkey
}
