// Adapted yellowstone-grpc/yellowstone-grpc-geyser/src/grpc.rs

use geyser_grpc_proto::{
    convert_to,
    prelude::{
        subscribe_update::UpdateOneof, CommitmentLevel, SubscribeUpdateAccount,
        SubscribeUpdateAccountInfo, SubscribeUpdateBlock,
        SubscribeUpdateBlockMeta, SubscribeUpdateEntry, SubscribeUpdateSlot,
        SubscribeUpdateTransaction, SubscribeUpdateTransactionInfo,
    },
};
use magicblock_transaction_status::{Reward, TransactionStatusMeta};
use solana_geyser_plugin_interface::geyser_plugin_interface::{
    ReplicaAccountInfoV3, ReplicaBlockInfoV3, ReplicaEntryInfoV2,
    ReplicaTransactionInfoV2, SlotStatus,
};
use solana_sdk::{
    account::ReadableAccount, clock::UnixTimestamp, pubkey::Pubkey,
    signature::Signature, transaction::SanitizedTransaction,
};

use crate::filters::FilterAccountsDataSlice;

#[derive(Debug, Clone)]
pub struct MessageAccountInfo {
    pub pubkey: Pubkey,
    pub lamports: u64,
    pub owner: Pubkey,
    pub executable: bool,
    pub rent_epoch: u64,
    pub data: Vec<u8>,
    pub write_version: u64,
    pub txn_signature: Option<Signature>,
}

impl ReadableAccount for MessageAccountInfo {
    fn data(&self) -> &[u8] {
        &self.data
    }
    fn owner(&self) -> &Pubkey {
        &self.owner
    }
    fn lamports(&self) -> u64 {
        self.lamports
    }
    fn executable(&self) -> bool {
        self.executable
    }
    fn rent_epoch(&self) -> solana_sdk::clock::Epoch {
        self.rent_epoch
    }
}

impl MessageAccountInfo {
    fn to_proto(
        &self,
        accounts_data_slice: &[FilterAccountsDataSlice],
    ) -> SubscribeUpdateAccountInfo {
        let data = if accounts_data_slice.is_empty() {
            self.data.clone()
        } else {
            let mut data = Vec::with_capacity(
                accounts_data_slice.iter().map(|ds| ds.length).sum(),
            );
            for data_slice in accounts_data_slice {
                if self.data.len() >= data_slice.end {
                    data.extend_from_slice(
                        &self.data[data_slice.start..data_slice.end],
                    );
                }
            }
            data
        };
        SubscribeUpdateAccountInfo {
            pubkey: self.pubkey.as_ref().into(),
            lamports: self.lamports,
            owner: self.owner.as_ref().into(),
            executable: self.executable,
            rent_epoch: self.rent_epoch,
            data,
            write_version: self.write_version,
            txn_signature: self.txn_signature.map(|s| s.as_ref().into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageAccount {
    pub account: MessageAccountInfo,
    pub slot: u64,
    pub is_startup: bool,
}

impl<'a> From<(&'a ReplicaAccountInfoV3<'a>, u64, bool)> for MessageAccount {
    fn from(
        (account, slot, is_startup): (&'a ReplicaAccountInfoV3<'a>, u64, bool),
    ) -> Self {
        Self {
            account: MessageAccountInfo {
                pubkey: Pubkey::try_from(account.pubkey).expect("valid Pubkey"),
                lamports: account.lamports,
                owner: Pubkey::try_from(account.owner).expect("valid Pubkey"),
                executable: account.executable,
                rent_epoch: account.rent_epoch,
                data: account.data.into(),
                write_version: account.write_version,
                txn_signature: account.txn.map(|txn| *txn.signature()),
            },
            slot,
            is_startup,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MessageSlot {
    pub slot: u64,
    pub parent: Option<u64>,
    pub status: CommitmentLevel,
}

impl From<(u64, Option<u64>, SlotStatus)> for MessageSlot {
    fn from((slot, parent, status): (u64, Option<u64>, SlotStatus)) -> Self {
        Self {
            slot,
            parent,
            // this BS is pretty much irrelevant in ER
            status: match status {
                SlotStatus::Processed | SlotStatus::FirstShredReceived => {
                    CommitmentLevel::Processed
                }
                SlotStatus::Confirmed | SlotStatus::CreatedBank => {
                    CommitmentLevel::Confirmed
                }
                SlotStatus::Rooted
                | SlotStatus::Completed
                | SlotStatus::Dead(_) => CommitmentLevel::Finalized,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageTransactionInfo {
    pub signature: Signature,
    pub is_vote: bool,
    pub transaction: SanitizedTransaction,
    pub meta: TransactionStatusMeta,
    pub index: usize,
}

impl MessageTransactionInfo {
    fn to_proto(&self) -> SubscribeUpdateTransactionInfo {
        SubscribeUpdateTransactionInfo {
            signature: self.signature.as_ref().into(),
            is_vote: self.is_vote,
            transaction: Some(convert_to::create_transaction(
                &self.transaction,
            )),
            meta: Some(convert_to::create_transaction_meta(&self.meta)),
            index: self.index as u64,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageTransaction {
    pub transaction: MessageTransactionInfo,
    pub slot: u64,
}

impl<'a> From<(&'a ReplicaTransactionInfoV2<'a>, u64)> for MessageTransaction {
    fn from(
        (transaction, slot): (&'a ReplicaTransactionInfoV2<'a>, u64),
    ) -> Self {
        Self {
            transaction: MessageTransactionInfo {
                signature: *transaction.signature,
                is_vote: transaction.is_vote,
                transaction: transaction.transaction.clone(),
                meta: transaction.transaction_status_meta.clone(),
                index: transaction.index,
            },
            slot,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageEntry {
    pub slot: u64,
    pub index: usize,
    pub num_hashes: u64,
    pub hash: Vec<u8>,
    pub executed_transaction_count: u64,
    pub starting_transaction_index: u64,
}

impl From<&ReplicaEntryInfoV2<'_>> for MessageEntry {
    fn from(entry: &ReplicaEntryInfoV2) -> Self {
        Self {
            slot: entry.slot,
            index: entry.index,
            num_hashes: entry.num_hashes,
            hash: entry.hash.into(),
            executed_transaction_count: entry.executed_transaction_count,
            starting_transaction_index: entry
                .starting_transaction_index
                .try_into()
                .expect("failed convert usize to u64"),
        }
    }
}

impl MessageEntry {
    fn to_proto(&self) -> SubscribeUpdateEntry {
        SubscribeUpdateEntry {
            slot: self.slot,
            index: self.index as u64,
            num_hashes: self.num_hashes,
            hash: self.hash.clone(),
            executed_transaction_count: self.executed_transaction_count,
            starting_transaction_index: self.starting_transaction_index,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageBlock {
    pub parent_slot: u64,
    pub slot: u64,
    pub parent_blockhash: String,
    pub blockhash: String,
    pub rewards: Vec<Reward>,
    pub block_time: Option<UnixTimestamp>,
    pub block_height: Option<u64>,
    pub executed_transaction_count: u64,
    pub transactions: Vec<MessageTransactionInfo>,
    pub updated_account_count: u64,
    pub accounts: Vec<MessageAccountInfo>,
    pub entries_count: u64,
    pub entries: Vec<MessageEntry>,
}

impl
    From<(
        MessageBlockMeta,
        Vec<MessageTransactionInfo>,
        Vec<MessageAccountInfo>,
        Vec<MessageEntry>,
    )> for MessageBlock
{
    fn from(
        (blockinfo, transactions, accounts, entries): (
            MessageBlockMeta,
            Vec<MessageTransactionInfo>,
            Vec<MessageAccountInfo>,
            Vec<MessageEntry>,
        ),
    ) -> Self {
        Self {
            parent_slot: blockinfo.parent_slot,
            slot: blockinfo.slot,
            blockhash: blockinfo.blockhash,
            parent_blockhash: blockinfo.parent_blockhash,
            rewards: blockinfo.rewards,
            block_time: blockinfo.block_time,
            block_height: blockinfo.block_height,
            executed_transaction_count: blockinfo.executed_transaction_count,
            transactions,
            updated_account_count: accounts.len() as u64,
            accounts,
            entries_count: entries.len() as u64,
            entries,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageBlockMeta {
    pub parent_slot: u64,
    pub slot: u64,
    pub parent_blockhash: String,
    pub blockhash: String,
    pub rewards: Vec<Reward>,
    pub block_time: Option<UnixTimestamp>,
    pub block_height: Option<u64>,
    pub executed_transaction_count: u64,
    pub entries_count: u64,
}

impl<'a> From<&'a ReplicaBlockInfoV3<'a>> for MessageBlockMeta {
    fn from(blockinfo: &'a ReplicaBlockInfoV3<'a>) -> Self {
        Self {
            parent_slot: blockinfo.parent_slot,
            slot: blockinfo.slot,
            parent_blockhash: blockinfo.parent_blockhash.to_string(),
            blockhash: blockinfo.blockhash.to_string(),
            rewards: blockinfo.rewards.into(),
            block_time: blockinfo.block_time,
            block_height: blockinfo.block_height,
            executed_transaction_count: blockinfo.executed_transaction_count,
            entries_count: blockinfo.entry_count,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Message {
    Slot(MessageSlot),
    Account(MessageAccount),
    Transaction(MessageTransaction),
    Entry(MessageEntry),
    Block(MessageBlock),
    BlockMeta(MessageBlockMeta),
}

impl Message {
    pub const fn get_slot(&self) -> u64 {
        match self {
            Self::Slot(msg) => msg.slot,
            Self::Account(msg) => msg.slot,
            Self::Transaction(msg) => msg.slot,
            Self::Entry(msg) => msg.slot,
            Self::Block(msg) => msg.slot,
            Self::BlockMeta(msg) => msg.slot,
        }
    }

    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Slot(_) => "Slot",
            Self::Account(_) => "Account",
            Self::Transaction(_) => "Transaction",
            Self::Entry(_) => "Entry",
            Self::Block(_) => "Block",
            Self::BlockMeta(_) => "BlockMeta",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageBlockRef<'a> {
    pub parent_slot: u64,
    pub slot: u64,
    pub parent_blockhash: &'a String,
    pub blockhash: &'a String,
    pub rewards: &'a Vec<Reward>,
    pub block_time: Option<UnixTimestamp>,
    pub block_height: Option<u64>,
    pub executed_transaction_count: u64,
    pub transactions: Vec<&'a MessageTransactionInfo>,
    pub updated_account_count: u64,
    pub accounts: Vec<&'a MessageAccountInfo>,
    pub entries_count: u64,
    pub entries: Vec<&'a MessageEntry>,
}

impl<'a>
    From<(
        &'a MessageBlock,
        Vec<&'a MessageTransactionInfo>,
        Vec<&'a MessageAccountInfo>,
        Vec<&'a MessageEntry>,
    )> for MessageBlockRef<'a>
{
    fn from(
        (block, transactions, accounts, entries): (
            &'a MessageBlock,
            Vec<&'a MessageTransactionInfo>,
            Vec<&'a MessageAccountInfo>,
            Vec<&'a MessageEntry>,
        ),
    ) -> Self {
        Self {
            parent_slot: block.parent_slot,
            slot: block.slot,
            parent_blockhash: &block.parent_blockhash,
            blockhash: &block.blockhash,
            rewards: &block.rewards,
            block_time: block.block_time,
            block_height: block.block_height,
            executed_transaction_count: block.executed_transaction_count,
            transactions,
            updated_account_count: block.updated_account_count,
            accounts,
            entries_count: block.entries_count,
            entries,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum MessageRef<'a> {
    Slot(&'a MessageSlot),
    Account(&'a MessageAccount),
    Transaction(&'a MessageTransaction),
    Entry(&'a MessageEntry),
    Block(MessageBlockRef<'a>),
    BlockMeta(&'a MessageBlockMeta),
}

impl MessageRef<'_> {
    pub fn to_proto(
        &self,
        accounts_data_slice: &[FilterAccountsDataSlice],
    ) -> UpdateOneof {
        match self {
            Self::Slot(message) => UpdateOneof::Slot(SubscribeUpdateSlot {
                slot: message.slot,
                parent: message.parent,
                status: message.status as i32,
            }),
            Self::Account(message) => {
                UpdateOneof::Account(SubscribeUpdateAccount {
                    account: Some(
                        message.account.to_proto(accounts_data_slice),
                    ),
                    slot: message.slot,
                    is_startup: message.is_startup,
                })
            }
            Self::Transaction(message) => {
                UpdateOneof::Transaction(SubscribeUpdateTransaction {
                    transaction: Some(message.transaction.to_proto()),
                    slot: message.slot,
                })
            }
            Self::Entry(message) => UpdateOneof::Entry(message.to_proto()),
            Self::Block(message) => UpdateOneof::Block(SubscribeUpdateBlock {
                slot: message.slot,
                blockhash: message.blockhash.clone(),
                rewards: Some(convert_to::create_rewards_obj(
                    message.rewards.as_slice(),
                )),
                block_time: message
                    .block_time
                    .map(convert_to::create_timestamp),
                block_height: message
                    .block_height
                    .map(convert_to::create_block_height),
                parent_slot: message.parent_slot,
                parent_blockhash: message.parent_blockhash.clone(),
                executed_transaction_count: message.executed_transaction_count,
                transactions: message
                    .transactions
                    .iter()
                    .map(|tx| tx.to_proto())
                    .collect(),
                updated_account_count: message.updated_account_count,
                accounts: message
                    .accounts
                    .iter()
                    .map(|acc| acc.to_proto(accounts_data_slice))
                    .collect(),
                entries_count: message.entries_count,
                entries: message
                    .entries
                    .iter()
                    .map(|entry| entry.to_proto())
                    .collect(),
            }),
            Self::BlockMeta(message) => {
                UpdateOneof::BlockMeta(SubscribeUpdateBlockMeta {
                    slot: message.slot,
                    blockhash: message.blockhash.clone(),
                    rewards: Some(convert_to::create_rewards_obj(
                        message.rewards.as_slice(),
                    )),
                    block_time: message
                        .block_time
                        .map(convert_to::create_timestamp),
                    block_height: message
                        .block_height
                        .map(convert_to::create_block_height),
                    parent_slot: message.parent_slot,
                    parent_blockhash: message.parent_blockhash.clone(),
                    executed_transaction_count: message
                        .executed_transaction_count,
                    entries_count: message.entries_count,
                })
            }
        }
    }
}
