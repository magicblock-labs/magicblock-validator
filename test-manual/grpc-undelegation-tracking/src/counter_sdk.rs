#![allow(deprecated)]

use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey,
    pubkey::Pubkey,
    system_program,
};

pub const FLEXI_COUNTER_PROGRAM_ID: Pubkey =
    pubkey!("f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4");
pub const DELEGATION_PROGRAM_ID: Pubkey =
    pubkey!("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
pub const MAGIC_PROGRAM_ID: Pubkey =
    pubkey!("Magic11111111111111111111111111111111111111");
pub const MAGIC_CONTEXT_PUBKEY: Pubkey =
    pubkey!("MagicContext1111111111111111111111111111111");

const FLEXI_COUNTER_SEED: &[u8] = b"flexi_counter";
const DELEGATION_METADATA_SEED: &[u8] = b"delegation-metadata";
const BUFFER_SEED: &[u8] = b"buffer";

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct DelegateArgs {
    pub valid_until: i64,
    pub commit_frequency_ms: u32,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub enum FlexiCounterInstruction {
    Init { label: String, bump: u8 },
    Realloc { bytes: u64, invocation_count: u16 },
    Add { count: u8 },
    AddUnsigned { count: u8 },
    AddError { count: u8 },
    Mul { multiplier: u8 },
    Delegate(DelegateArgs),
    AddAndScheduleCommit { count: u8, undelegate: bool },
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct FlexiCounter {
    pub count: u64,
    pub updates: u64,
    pub label: String,
}

impl FlexiCounter {
    pub fn try_decode(data: &[u8]) -> std::io::Result<Self> {
        Self::try_from_slice(data)
    }
}

pub fn derive_counter_pda(payer: &Pubkey) -> (Pubkey, u8) {
    let seeds = [
        FLEXI_COUNTER_PROGRAM_ID.as_ref(),
        FLEXI_COUNTER_SEED,
        payer.as_ref(),
    ];
    Pubkey::find_program_address(&seeds, &FLEXI_COUNTER_PROGRAM_ID)
}

pub fn derive_delegation_record(delegated_account: &Pubkey) -> Pubkey {
    dlp::pda::delegation_record_pda_from_delegated_account(delegated_account)
}

pub fn derive_delegation_metadata(delegated_account: &Pubkey) -> Pubkey {
    let (pda, _) = Pubkey::find_program_address(
        &[DELEGATION_METADATA_SEED, delegated_account.as_ref()],
        &DELEGATION_PROGRAM_ID,
    );
    pda
}

pub fn derive_delegate_buffer(
    delegated_account: &Pubkey,
    owner_program: &Pubkey,
) -> Pubkey {
    let (pda, _) = Pubkey::find_program_address(
        &[BUFFER_SEED, delegated_account.as_ref()],
        owner_program,
    );
    pda
}

pub fn build_init_ix(payer: &Pubkey, label: &str) -> Instruction {
    let (pda, bump) = derive_counter_pda(payer);
    let accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(pda, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    Instruction::new_with_borsh(
        FLEXI_COUNTER_PROGRAM_ID,
        &FlexiCounterInstruction::Init {
            label: label.to_string(),
            bump,
        },
        accounts,
    )
}

pub fn build_delegate_ix(
    payer: &Pubkey,
    validator: Option<&Pubkey>,
) -> Instruction {
    let (pda, _) = derive_counter_pda(payer);

    let delegation_record = derive_delegation_record(&pda);
    let delegation_metadata = derive_delegation_metadata(&pda);
    let delegate_buffer =
        derive_delegate_buffer(&pda, &FLEXI_COUNTER_PROGRAM_ID);

    let mut account_metas = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(pda, false),
        AccountMeta::new_readonly(FLEXI_COUNTER_PROGRAM_ID, false),
        AccountMeta::new(delegate_buffer, false),
        AccountMeta::new(delegation_record, false),
        AccountMeta::new(delegation_metadata, false),
        AccountMeta::new_readonly(DELEGATION_PROGRAM_ID, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    if let Some(v) = validator {
        account_metas.push(AccountMeta::new_readonly(*v, false));
    }

    let args = DelegateArgs {
        valid_until: i64::MAX,
        commit_frequency_ms: 0,
    };

    Instruction::new_with_borsh(
        FLEXI_COUNTER_PROGRAM_ID,
        &FlexiCounterInstruction::Delegate(args),
        account_metas,
    )
}

pub fn build_add_and_schedule_commit_ix(
    payer: &Pubkey,
    count: u8,
    undelegate: bool,
) -> Instruction {
    let (pda, _) = derive_counter_pda(payer);
    let accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(pda, false),
        AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
        AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
    ];
    Instruction::new_with_borsh(
        FLEXI_COUNTER_PROGRAM_ID,
        &FlexiCounterInstruction::AddAndScheduleCommit { count, undelegate },
        accounts,
    )
}

pub fn read_counter_value(
    rpc: &solana_rpc_client::rpc_client::RpcClient,
    payer: &Pubkey,
) -> anyhow::Result<FlexiCounter> {
    let (pda, _) = derive_counter_pda(payer);
    let account = rpc.get_account(&pda)?;
    let counter = FlexiCounter::try_decode(&account.data)?;
    Ok(counter)
}
