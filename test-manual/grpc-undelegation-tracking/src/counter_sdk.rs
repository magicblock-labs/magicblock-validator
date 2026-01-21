#![allow(deprecated)]

use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::{
    hash::hash,
    instruction::{AccountMeta, Instruction},
    pubkey,
    pubkey::Pubkey,
    system_program,
};

pub const COUNTER_PROGRAM_ID: Pubkey =
    pubkey!("3tXjp4SobUQMJcV5KcwBUykWSizkMq6qLTSuSZiU9Ajd");
pub const DELEGATION_PROGRAM_ID: Pubkey =
    pubkey!("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
pub const MAGIC_PROGRAM_ID: Pubkey =
    pubkey!("Magic11111111111111111111111111111111111111");
pub const MAGIC_CONTEXT_PUBKEY: Pubkey =
    pubkey!("MagicContext1111111111111111111111111111111");

const COUNTER_SEED: &[u8] = b"counter";
const DELEGATION_METADATA_SEED: &[u8] = b"delegation-metadata";
const BUFFER_SEED: &[u8] = b"buffer";

const ANCHOR_DISCRIMINATOR_LEN: usize = 8;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct Counter {
    pub count: u64,
}

impl Counter {
    pub fn try_decode(data: &[u8]) -> std::io::Result<Self> {
        if data.len() < ANCHOR_DISCRIMINATOR_LEN + 8 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Account data too short for Counter",
            ));
        }
        Self::try_from_slice(&data[ANCHOR_DISCRIMINATOR_LEN..])
    }
}

fn anchor_discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("global:{}", name);
    let h = hash(preimage.as_bytes());
    h.to_bytes()[..8].try_into().unwrap()
}

pub fn derive_counter_pda(counter_id: u64) -> (Pubkey, u8) {
    let id_bytes = counter_id.to_le_bytes();
    let seeds = [COUNTER_SEED, id_bytes.as_ref()];
    Pubkey::find_program_address(&seeds, &COUNTER_PROGRAM_ID)
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

pub fn build_init_ix(payer: &Pubkey, counter_id: u64) -> Instruction {
    let (pda, _) = derive_counter_pda(counter_id);
    let accounts = vec![
        AccountMeta::new(pda, false),
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    let discriminator = anchor_discriminator("initialize");
    let mut data = discriminator.to_vec();
    data.extend_from_slice(&counter_id.to_le_bytes());

    Instruction::new_with_bytes(COUNTER_PROGRAM_ID, &data, accounts)
}

pub fn build_delegate_ix(
    payer: &Pubkey,
    counter_id: u64,
    validator: Option<&Pubkey>,
) -> Instruction {
    let (pda, _) = derive_counter_pda(counter_id);

    let delegation_record = derive_delegation_record(&pda);
    let delegation_metadata = derive_delegation_metadata(&pda);
    let delegate_buffer = derive_delegate_buffer(&pda, &COUNTER_PROGRAM_ID);

    // Account order must match the IDL (after macro expansion):
    // 1. payer (signer)
    // 2. buffer_pda (writable)
    // 3. delegation_record_pda (writable)
    // 4. delegation_metadata_pda (writable)
    // 5. pda (writable) - the account being delegated
    // 6. owner_program (readonly)
    // 7. delegation_program (readonly)
    // 8. system_program (readonly)
    // 9+ remaining_accounts (validator if specified)
    let mut account_metas = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(delegate_buffer, false),
        AccountMeta::new(delegation_record, false),
        AccountMeta::new(delegation_metadata, false),
        AccountMeta::new(pda, false),
        AccountMeta::new_readonly(COUNTER_PROGRAM_ID, false),
        AccountMeta::new_readonly(DELEGATION_PROGRAM_ID, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    if let Some(v) = validator {
        account_metas.push(AccountMeta::new_readonly(*v, false));
    }

    let discriminator = anchor_discriminator("delegate");
    let mut data = discriminator.to_vec();
    data.extend_from_slice(&counter_id.to_le_bytes());

    Instruction::new_with_bytes(COUNTER_PROGRAM_ID, &data, account_metas)
}

pub fn build_increment_and_undelegate_ix(
    payer: &Pubkey,
    counter_id: u64,
) -> Instruction {
    let (pda, _) = derive_counter_pda(counter_id);
    let accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(pda, false),
        AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
        AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
    ];

    let discriminator = anchor_discriminator("increment_and_undelegate");
    let mut data = discriminator.to_vec();
    data.extend_from_slice(&counter_id.to_le_bytes());

    Instruction::new_with_bytes(COUNTER_PROGRAM_ID, &data, accounts)
}

pub fn read_counter_value(
    rpc: &solana_rpc_client::rpc_client::RpcClient,
    counter_id: u64,
) -> anyhow::Result<Counter> {
    let (pda, _) = derive_counter_pda(counter_id);
    let account = rpc.get_account(&pda)?;
    let counter = Counter::try_decode(&account.data)?;
    Ok(counter)
}
