use log::*;
use serde::Deserialize;
use solana_account_decoder::parse_token::UiTokenAmount;
use solana_sdk::{
    clock::{Slot, UnixTimestamp},
    hash::Hash,
    instruction::CompiledInstruction,
    message::{
        v0::{self, LoadedAddresses},
        MessageHeader, VersionedMessage,
    },
    pubkey::Pubkey,
    signature::Signature,
    transaction::{self, TransactionError, VersionedTransaction},
    transaction_context::TransactionReturnData,
};
use solana_storage_proto::convert::generated;
use solana_transaction_status::{
    ConfirmedTransactionWithStatusMeta, InnerInstruction, InnerInstructions,
    Reward, RewardType, TransactionStatusMeta, TransactionTokenBalance,
    TransactionWithStatusMeta, VersionedTransactionWithStatusMeta,
};

pub fn from_generated_confirmed_transaction(
    slot: Slot,
    tx: generated::ConfirmedTransaction,
    block_time: Option<UnixTimestamp>,
) -> ConfirmedTransactionWithStatusMeta {
    let tx_with_meta = tx_with_meta_from_generated(tx);
    ConfirmedTransactionWithStatusMeta {
        slot,
        block_time,
        tx_with_meta,
    }
}
fn tx_with_meta_from_generated(
    tx: generated::ConfirmedTransaction,
) -> TransactionWithStatusMeta {
    let meta = tx.meta.map(tx_meta_from_generated);
    let transaction = tx.transaction.map(tx_from_generated).expect(
        "Never should store confirmed transaction without a transaction",
    );

    use TransactionWithStatusMeta::*;
    match meta {
        Some(meta) => {
            Complete(VersionedTransactionWithStatusMeta { transaction, meta })
        }
        // TODO: @@@ledger
        None => todo!(), // MissingMetadata(transaction),
    }
}

// -----------------
// Transaction Conversions
// -----------------
fn tx_from_generated(tx: generated::Transaction) -> VersionedTransaction {
    let message = tx.message.map(message_from_generated).unwrap_or_default();
    let signatures = signatures_from_slices(tx.signatures);
    VersionedTransaction {
        signatures,
        message,
    }
}

fn message_from_generated(msg: generated::Message) -> VersionedMessage {
    let account_keys = pubkeys_from_slices(msg.account_keys);
    let recent_blockhash = Hash::new(&msg.recent_blockhash);
    let message = v0::Message {
        header: msg.header.map(header_from_generated).unwrap_or_default(),
        recent_blockhash,
        account_keys,
        instructions: msg
            .instructions
            .into_iter()
            .map(compiled_instruction_from_generated)
            .collect(),
        address_table_lookups: msg
            .address_table_lookups
            .into_iter()
            .flat_map(try_address_table_lookup_from_generated)
            .collect(),
    };
    VersionedMessage::V0(message)
}

fn header_from_generated(header: generated::MessageHeader) -> MessageHeader {
    MessageHeader {
        num_required_signatures: header.num_required_signatures as u8,
        num_readonly_signed_accounts: header.num_readonly_signed_accounts as u8,
        num_readonly_unsigned_accounts: header.num_readonly_unsigned_accounts
            as u8,
    }
}

fn compiled_instruction_from_generated(
    instruction: generated::CompiledInstruction,
) -> CompiledInstruction {
    let program_id_index = instruction.program_id_index as u8;
    let accounts = instruction.accounts;
    let data = instruction.data;
    CompiledInstruction {
        program_id_index,
        accounts,
        data,
    }
}

fn try_address_table_lookup_from_generated(
    lookup: generated::MessageAddressTableLookup,
) -> Option<v0::MessageAddressTableLookup> {
    let account_key = match Pubkey::try_from(lookup.account_key) {
        Ok(pubkey) => pubkey,
        Err(err) => {
            warn!("Invalid pubkey: {:?}", err);
            return None;
        }
    };
    let writable_indexes = lookup.writable_indexes;
    let readonly_indexes = lookup.readonly_indexes;
    Some(v0::MessageAddressTableLookup {
        account_key,
        writable_indexes,
        readonly_indexes,
    })
}

fn signatures_from_slices(signatures: Vec<Vec<u8>>) -> Vec<Signature> {
    signatures
        .into_iter()
        .flat_map(|slice| {
            Signature::try_from(slice.as_slice())
                .map_err(|e| {
                    warn!("Invalid signature: {:?}", e);
                    e
                })
                .ok()
        })
        .collect()
}

// -----------------
// TransactionStatus Meta Conversions
// -----------------
fn tx_meta_from_generated(
    meta: generated::TransactionStatusMeta,
) -> solana_transaction_status::TransactionStatusMeta {
    let inner_instructions =
        inner_instructions_from_generated(meta.inner_instructions);
    let rewards = rewards_from_generated(meta.rewards);
    let pre_token_balances =
        token_balances_from_generated(meta.pre_token_balances);
    let post_token_balances =
        token_balances_from_generated(meta.post_token_balances);
    let status = status_from_generated(meta.err);
    let return_data = return_data_from_generated(meta.return_data);
    TransactionStatusMeta {
        status,
        compute_units_consumed: meta.compute_units_consumed,
        loaded_addresses: LoadedAddresses {
            writable: pubkeys_from_slices(meta.loaded_writable_addresses),
            readonly: pubkeys_from_slices(meta.loaded_readonly_addresses),
        },
        fee: meta.fee,
        pre_balances: meta.pre_balances,
        post_balances: meta.post_balances,
        inner_instructions: Some(inner_instructions),
        log_messages: Some(meta.log_messages),
        pre_token_balances: Some(pre_token_balances),
        post_token_balances: Some(post_token_balances),
        return_data,
        rewards: Some(rewards),
    }
}

fn status_from_generated(
    err: Option<generated::TransactionError>,
) -> transaction::Result<()> {
    match err {
        None => Ok(()),
        Some(err) => {
            let e: Option<TransactionError> = bincode::deserialize(&err.err)
                .map_err(|e| {
                    warn!("Invalid transaction error: {:?}", e);
                    e
                })
                .ok();

            match e {
                Some(err) => transaction::Result::Err(err),
                None => transaction::Result::Ok(()),
            }
        }
    }
}

fn inner_instructions_from_generated(
    inner_instructions: Vec<generated::InnerInstructions>,
) -> Vec<InnerInstructions> {
    inner_instructions
        .into_iter()
        .map(|inner_instructions| InnerInstructions {
            index: inner_instructions.index as u8,
            instructions: inner_instructions
                .instructions
                .into_iter()
                .map(|ix| {
                    let stack_height = ix.stack_height();
                    InnerInstruction {
                        instruction: CompiledInstruction {
                            program_id_index: ix.program_id_index as u8,
                            accounts: ix.accounts,
                            data: ix.data,
                        },
                        stack_height: Some(stack_height),
                    }
                })
                .collect(),
        })
        .collect()
}

fn pubkeys_from_slices(pubkeys: Vec<Vec<u8>>) -> Vec<Pubkey> {
    pubkeys
        .into_iter()
        .flat_map(|slice| {
            Pubkey::try_from(slice)
                .map_err(|e| {
                    warn!("Invalid pubkey: {:?}", e);
                    e
                })
                .ok()
        })
        .collect()
}

fn token_balances_from_generated(
    token_balances: Vec<generated::TokenBalance>,
) -> Vec<TransactionTokenBalance> {
    token_balances
        .into_iter()
        .map(|tb| {
            let ui_token_amount = tb
                .ui_token_amount
                .map(token_amount_from_generated)
                .unwrap_or(UiTokenAmount {
                    ui_amount: None,
                    decimals: Default::default(),
                    amount: Default::default(),
                    ui_amount_string: Default::default(),
                });
            TransactionTokenBalance {
                account_index: tb.account_index as u8,
                mint: tb.mint,
                ui_token_amount,
                owner: tb.owner,
                program_id: tb.program_id,
            }
        })
        .collect()
}

fn token_amount_from_generated(
    token_amount: generated::UiTokenAmount,
) -> UiTokenAmount {
    UiTokenAmount {
        ui_amount: Some(token_amount.ui_amount),
        decimals: token_amount.decimals as u8,
        amount: token_amount.amount,
        ui_amount_string: token_amount.ui_amount_string,
    }
}

fn rewards_from_generated(rewards: Vec<generated::Reward>) -> Vec<Reward> {
    rewards
        .into_iter()
        .map(|r| Reward {
            pubkey: r.pubkey,
            lamports: r.lamports,
            post_balance: r.post_balance,
            reward_type: reward_type_from(r.reward_type),
            // NOTE: we don't support votes nor staking
            commission: None,
        })
        .collect()
}

fn reward_type_from(n: i32) -> Option<RewardType> {
    use RewardType::*;
    match n {
        0 => Some(Fee),
        1 => Some(Rent),
        2 => Some(Staking),
        3 => Some(Voting),
        _ => None,
    }
}

fn return_data_from_generated(
    data: Option<generated::ReturnData>,
) -> Option<TransactionReturnData> {
    match data {
        None => None,
        Some(data) => match Pubkey::try_from(data.program_id) {
            Err(e) => {
                warn!("Invalid pubkey: {:?}", e);
                None
            }
            Ok(program_id) => Some(TransactionReturnData {
                program_id,
                data: data.data,
            }),
        },
    }
}
