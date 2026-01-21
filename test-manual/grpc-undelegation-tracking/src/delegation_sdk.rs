#![allow(deprecated)]

use anyhow::{anyhow, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

pub const DELEGATION_PROGRAM_ID: Pubkey =
    solana_sdk::pubkey!("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");

const DISCRIMINATOR_SIZE: usize = 8;

#[derive(Debug, Clone, Copy, BorshSerialize, BorshDeserialize)]
pub struct DelegationRecord {
    pub authority: Pubkey,
    pub owner: Pubkey,
    pub delegation_slot: u64,
    pub lamports: u64,
    pub commit_frequency_ms: u64,
}

#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
pub struct DelegationMetadata {
    pub last_update_nonce: u64,
    pub is_undelegatable: bool,
    pub seeds: Vec<Vec<u8>>,
    pub rent_payer: Pubkey,
}

#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
pub struct CommitStateArgs {
    pub nonce: u64,
    pub lamports: u64,
    pub allow_undelegation: bool,
    pub data: Vec<u8>,
}

pub fn delegation_record_pda(delegated_account: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"delegation", delegated_account.as_ref()],
        &DELEGATION_PROGRAM_ID,
    )
    .0
}

pub fn delegation_metadata_pda(delegated_account: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"delegation-metadata", delegated_account.as_ref()],
        &DELEGATION_PROGRAM_ID,
    )
    .0
}

pub fn commit_state_pda(delegated_account: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"state-diff", delegated_account.as_ref()],
        &DELEGATION_PROGRAM_ID,
    )
    .0
}

pub fn commit_record_pda(delegated_account: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"commit-state-record", delegated_account.as_ref()],
        &DELEGATION_PROGRAM_ID,
    )
    .0
}

pub fn undelegate_buffer_pda(delegated_account: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"undelegate-buffer", delegated_account.as_ref()],
        &DELEGATION_PROGRAM_ID,
    )
    .0
}

pub fn fees_vault_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"fees-vault"], &DELEGATION_PROGRAM_ID).0
}

pub fn validator_fees_vault_pda(validator: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"v-fees-vault", validator.as_ref()],
        &DELEGATION_PROGRAM_ID,
    )
    .0
}

pub fn program_config_pda(owner_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"p-conf", owner_program.as_ref()],
        &DELEGATION_PROGRAM_ID,
    )
    .0
}

pub fn fetch_delegation_record(
    rpc: &solana_rpc_client::rpc_client::RpcClient,
    delegated_account: &Pubkey,
) -> Result<DelegationRecord> {
    let pda = delegation_record_pda(delegated_account);
    let account = rpc
        .get_account(&pda)
        .map_err(|e| anyhow!("Failed to fetch delegation record: {}", e))?;

    if account.data.len() < DISCRIMINATOR_SIZE {
        return Err(anyhow!("Delegation record data too short"));
    }

    DelegationRecord::try_from_slice(&account.data[DISCRIMINATOR_SIZE..])
        .map_err(|e| anyhow!("Failed to deserialize delegation record: {}", e))
}

pub fn fetch_delegation_metadata(
    rpc: &solana_rpc_client::rpc_client::RpcClient,
    delegated_account: &Pubkey,
) -> Result<DelegationMetadata> {
    let pda = delegation_metadata_pda(delegated_account);
    let account = rpc
        .get_account(&pda)
        .map_err(|e| anyhow!("Failed to fetch delegation metadata: {}", e))?;

    if account.data.len() < DISCRIMINATOR_SIZE {
        return Err(anyhow!("Delegation metadata data too short"));
    }

    DelegationMetadata::try_from_slice(&account.data[DISCRIMINATOR_SIZE..])
        .map_err(|e| {
            anyhow!("Failed to deserialize delegation metadata: {}", e)
        })
}

const COMMIT_STATE_DISCRIMINATOR: u64 = 1;
const FINALIZE_DISCRIMINATOR: u64 = 2;
const UNDELEGATE_DISCRIMINATOR: u64 = 3;

pub fn build_commit_state_instruction(
    validator: Pubkey,
    delegated_account: Pubkey,
    owner_program: Pubkey,
    account_data: Vec<u8>,
    lamports: u64,
    nonce: u64,
    allow_undelegation: bool,
) -> Instruction {
    let args = CommitStateArgs {
        nonce,
        lamports,
        allow_undelegation,
        data: account_data,
    };

    let mut data = COMMIT_STATE_DISCRIMINATOR.to_le_bytes().to_vec();
    data.extend(borsh::to_vec(&args).unwrap());

    let accounts = vec![
        AccountMeta::new(validator, true),
        AccountMeta::new(delegated_account, false),
        AccountMeta::new_readonly(
            delegation_record_pda(&delegated_account),
            false,
        ),
        AccountMeta::new(delegation_metadata_pda(&delegated_account), false),
        AccountMeta::new(commit_state_pda(&delegated_account), false),
        AccountMeta::new(commit_record_pda(&delegated_account), false),
        AccountMeta::new_readonly(program_config_pda(&owner_program), false),
        AccountMeta::new(fees_vault_pda(), false),
        AccountMeta::new(validator_fees_vault_pda(&validator), false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    Instruction {
        program_id: DELEGATION_PROGRAM_ID,
        accounts,
        data,
    }
}

pub fn build_finalize_instruction(
    validator: Pubkey,
    delegated_account: Pubkey,
) -> Instruction {
    let data = FINALIZE_DISCRIMINATOR.to_le_bytes().to_vec();

    let accounts = vec![
        AccountMeta::new(validator, true),
        AccountMeta::new(delegated_account, false),
        AccountMeta::new_readonly(
            delegation_record_pda(&delegated_account),
            false,
        ),
        AccountMeta::new(delegation_metadata_pda(&delegated_account), false),
        AccountMeta::new(commit_state_pda(&delegated_account), false),
        AccountMeta::new(commit_record_pda(&delegated_account), false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    Instruction {
        program_id: DELEGATION_PROGRAM_ID,
        accounts,
        data,
    }
}

pub fn build_undelegate_instruction(
    validator: Pubkey,
    delegated_account: Pubkey,
    owner_program: Pubkey,
    rent_reimbursement: Pubkey,
) -> Instruction {
    let data = UNDELEGATE_DISCRIMINATOR.to_le_bytes().to_vec();

    let accounts = vec![
        AccountMeta::new(validator, true),
        AccountMeta::new(delegated_account, false),
        AccountMeta::new_readonly(
            delegation_record_pda(&delegated_account),
            false,
        ),
        AccountMeta::new(delegation_metadata_pda(&delegated_account), false),
        AccountMeta::new(commit_state_pda(&delegated_account), false),
        AccountMeta::new(commit_record_pda(&delegated_account), false),
        AccountMeta::new(undelegate_buffer_pda(&delegated_account), false),
        AccountMeta::new_readonly(owner_program, false),
        AccountMeta::new(rent_reimbursement, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    Instruction {
        program_id: DELEGATION_PROGRAM_ID,
        accounts,
        data,
    }
}

pub fn build_direct_undelegate_flow(
    validator: Pubkey,
    delegated_account: Pubkey,
    owner_program: Pubkey,
    rent_payer: Pubkey,
    account_data: Vec<u8>,
    lamports: u64,
    nonce: u64,
) -> Vec<Instruction> {
    vec![
        build_commit_state_instruction(
            validator,
            delegated_account,
            owner_program,
            account_data,
            lamports,
            nonce,
            true,
        ),
        build_finalize_instruction(validator, delegated_account),
        build_undelegate_instruction(
            validator,
            delegated_account,
            owner_program,
            rent_payer,
        ),
    ]
}
