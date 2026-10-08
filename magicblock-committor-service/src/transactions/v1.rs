use std::mem::size_of;

use serde::{ser::SerializeTuple, Serialize, Serializer};
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{
    compiled_instruction::CompiledInstruction, v0, CompileError, MessageHeader,
};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_signature::Signature;
use solana_signer::{Signer, SignerError};

const MESSAGE_VERSION_PREFIX: u8 = 0x80;
const V1_PREFIX: u8 = MESSAGE_VERSION_PREFIX | 1;
const MAX_ADDRESSES: usize = 64;
const MAX_INSTRUCTIONS: usize = 64;
const MAX_SIGNATURES: usize = 12;

const PRIORITY_FEE_MASK: u32 = 0b11;
const COMPUTE_UNIT_LIMIT_MASK: u32 = 0b100;
const LOADED_ACCOUNTS_DATA_SIZE_MASK: u32 = 0b1000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransactionConfig {
    pub priority_fee: Option<u64>,
    pub compute_unit_limit: Option<u32>,
    pub loaded_accounts_data_size_limit: Option<u32>,
}

impl TransactionConfig {
    pub const fn empty() -> Self {
        Self {
            priority_fee: None,
            compute_unit_limit: None,
            loaded_accounts_data_size_limit: None,
        }
    }

    #[must_use]
    pub const fn with_priority_fee(mut self, fee: u64) -> Self {
        self.priority_fee = Some(fee);
        self
    }

    #[must_use]
    pub const fn with_compute_unit_limit(mut self, limit: u32) -> Self {
        self.compute_unit_limit = Some(limit);
        self
    }

    #[must_use]
    pub const fn with_loaded_accounts_data_size_limit(
        mut self,
        limit: u32,
    ) -> Self {
        self.loaded_accounts_data_size_limit = Some(limit);
        self
    }

    fn mask(&self) -> u32 {
        let mut mask = 0;
        if self.priority_fee.is_some() {
            mask |= PRIORITY_FEE_MASK;
        }
        if self.compute_unit_limit.is_some() {
            mask |= COMPUTE_UNIT_LIMIT_MASK;
        }
        if self.loaded_accounts_data_size_limit.is_some() {
            mask |= LOADED_ACCOUNTS_DATA_SIZE_MASK;
        }
        mask
    }

    fn serialized_size(&self) -> usize {
        self.priority_fee.map_or(0, |_| size_of::<u64>())
            + self.compute_unit_limit.map_or(0, |_| size_of::<u32>())
            + self
                .loaded_accounts_data_size_limit
                .map_or(0, |_| size_of::<u32>())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    header: MessageHeader,
    config: TransactionConfig,
    account_keys: Vec<Pubkey>,
    recent_blockhash: Hash,
    instructions: Vec<CompiledInstruction>,
}

impl Message {
    pub(crate) fn try_compile_with_config(
        payer: &Pubkey,
        instructions: &[Instruction],
        recent_blockhash: Hash,
        config: TransactionConfig,
    ) -> Result<Self, CompileError> {
        let message = v0::Message::try_compile(
            payer,
            instructions,
            &[],
            recent_blockhash,
        )?;
        Ok(Self {
            header: message.header,
            config,
            account_keys: message.account_keys,
            recent_blockhash: message.recent_blockhash,
            instructions: message.instructions,
        })
    }

    pub(crate) fn set_recent_blockhash(&mut self, recent_blockhash: Hash) {
        self.recent_blockhash = recent_blockhash;
    }

    pub(crate) fn recent_blockhash(&self) -> &Hash {
        &self.recent_blockhash
    }

    pub(crate) fn matches_v0_message(&self, other: &v0::Message) -> bool {
        if self.config != TransactionConfig::empty() {
            return false;
        }
        other.address_table_lookups.is_empty()
            && self.header == other.header
            && self.account_keys == other.account_keys
            && self.recent_blockhash == other.recent_blockhash
            && self.instructions == other.instructions
    }

    pub(crate) fn validate(&self) -> Result<(), ()> {
        if usize::from(self.header.num_required_signatures) > MAX_SIGNATURES {
            return Err(());
        }
        if self.account_keys.len() > MAX_ADDRESSES {
            return Err(());
        }
        if self.instructions.len() > MAX_INSTRUCTIONS {
            return Err(());
        }

        let num_account_keys = self.account_keys.len();
        let min_account_keys = usize::from(self.header.num_required_signatures)
            .saturating_add(usize::from(
                self.header.num_readonly_unsigned_accounts,
            ));
        if num_account_keys < min_account_keys {
            return Err(());
        }
        if self.header.num_readonly_signed_accounts
            >= self.header.num_required_signatures
        {
            return Err(());
        }
        for (index, key) in self.account_keys.iter().enumerate() {
            if self.account_keys[..index].contains(key) {
                return Err(());
            }
        }

        let max_account_index = num_account_keys.checked_sub(1).ok_or(())?;
        for ix in &self.instructions {
            if usize::from(ix.program_id_index) > max_account_index {
                return Err(());
            }
            if ix.program_id_index == 0 {
                return Err(());
            }
            if ix.accounts.len() > u8::MAX as usize {
                return Err(());
            }
            if ix.data.len() > u16::MAX as usize {
                return Err(());
            }
            for &account_index in &ix.accounts {
                if usize::from(account_index) > max_account_index {
                    return Err(());
                }
            }
        }

        Ok(())
    }

    fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.serialized_size());
        out.push(V1_PREFIX);
        out.push(self.header.num_required_signatures);
        out.push(self.header.num_readonly_signed_accounts);
        out.push(self.header.num_readonly_unsigned_accounts);
        out.extend_from_slice(&self.config.mask().to_le_bytes());
        out.extend_from_slice(self.recent_blockhash.as_ref());
        out.push(self.instructions.len() as u8);
        out.push(self.account_keys.len() as u8);
        for key in &self.account_keys {
            out.extend_from_slice(key.as_ref());
        }
        if let Some(value) = self.config.priority_fee {
            out.extend_from_slice(&value.to_le_bytes());
        }
        if let Some(value) = self.config.compute_unit_limit {
            out.extend_from_slice(&value.to_le_bytes());
        }
        if let Some(value) = self.config.loaded_accounts_data_size_limit {
            out.extend_from_slice(&value.to_le_bytes());
        }
        for ix in &self.instructions {
            out.push(ix.program_id_index);
            out.push(ix.accounts.len() as u8);
            out.extend_from_slice(&(ix.data.len() as u16).to_le_bytes());
        }
        for ix in &self.instructions {
            out.extend_from_slice(&ix.accounts);
            out.extend_from_slice(&ix.data);
        }
        out
    }

    fn serialized_size(&self) -> usize {
        1 + 3
            + size_of::<u32>()
            + size_of::<Hash>()
            + 2
            + (self.account_keys.len() * size_of::<Pubkey>())
            + self.config.serialized_size()
            + self
                .instructions
                .iter()
                .map(|ix| 4 + ix.accounts.len() + ix.data.len())
                .sum::<usize>()
    }
}

pub(crate) struct Transaction {
    signature: Signature,
    recent_blockhash: Hash,
    serialized: Vec<u8>,
}

impl Transaction {
    pub(crate) fn try_new(
        message: Message,
        authority: &Keypair,
    ) -> Result<Self, SignerError> {
        let required_signatures =
            usize::from(message.header.num_required_signatures);
        if required_signatures != 1 {
            return Err(SignerError::NotEnoughSigners);
        }
        let authority_pubkey = authority.pubkey();
        if message.account_keys.first() != Some(&authority_pubkey) {
            return Err(SignerError::KeypairPubkeyMismatch);
        }

        let message_bytes = message.serialize();
        let signature = authority.try_sign_message(&message_bytes)?;
        let mut serialized = message_bytes;
        serialized.extend_from_slice(signature.as_ref());

        Ok(Self {
            signature,
            recent_blockhash: *message.recent_blockhash(),
            serialized,
        })
    }

    pub(crate) fn serialized_size(&self) -> usize {
        self.serialized.len()
    }
}

impl Serialize for Transaction {
    fn serialize<S: Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        // A tuple makes bincode emit the existing wire bytes without a length prefix.
        let mut tuple = serializer.serialize_tuple(self.serialized.len())?;
        for byte in &self.serialized {
            tuple.serialize_element(byte)?;
        }
        tuple.end()
    }
}

impl SerializableTransaction for Transaction {
    fn get_signature(&self) -> &Signature {
        &self.signature
    }

    fn get_recent_blockhash(&self) -> &Hash {
        &self.recent_blockhash
    }

    fn uses_durable_nonce(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_message() -> Message {
        Message {
            header: MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 1,
            },
            config: TransactionConfig::empty(),
            account_keys: vec![Pubkey::new_unique(), Pubkey::new_unique()],
            recent_blockhash: Hash::new_unique(),
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
                accounts: vec![0],
                data: vec![7, 8, 9],
            }],
        }
    }

    #[test]
    fn serializes_config_before_instruction_headers() {
        let config = TransactionConfig::empty()
            .with_priority_fee(11)
            .with_compute_unit_limit(22)
            .with_loaded_accounts_data_size_limit(33);
        let message = Message {
            header: MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 1,
            },
            config,
            account_keys: vec![Pubkey::new_unique(), Pubkey::new_unique()],
            recent_blockhash: Hash::new_unique(),
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
                accounts: vec![0],
                data: vec![7, 8, 9],
            }],
        };
        let serialized = message.serialize();
        let config_values_offset = 1
            + 3
            + size_of::<u32>()
            + size_of::<Hash>()
            + 2
            + (message.account_keys.len() * size_of::<Pubkey>());

        assert_eq!(
            &serialized[4..8],
            &(PRIORITY_FEE_MASK
                | COMPUTE_UNIT_LIMIT_MASK
                | LOADED_ACCOUNTS_DATA_SIZE_MASK)
                .to_le_bytes()
        );
        assert_eq!(
            &serialized[config_values_offset..config_values_offset + 8],
            &11u64.to_le_bytes()
        );
        assert_eq!(
            &serialized[config_values_offset + 8..config_values_offset + 12],
            &22u32.to_le_bytes()
        );
        assert_eq!(
            &serialized[config_values_offset + 12..config_values_offset + 16],
            &33u32.to_le_bytes()
        );
        assert_eq!(serialized[config_values_offset + 16], 1);
        assert_eq!(serialized.len(), message.serialized_size());
    }

    #[test]
    fn validate_accepts_valid_message() {
        assert!(valid_message().validate().is_ok());
    }

    #[test]
    fn validate_rejects_fee_payer_program() {
        let mut message = valid_message();
        message.instructions[0].program_id_index = 0;

        assert!(message.validate().is_err());
    }

    #[test]
    fn validate_rejects_invalid_account_index() {
        let mut message = valid_message();
        message.instructions[0].accounts = vec![2];

        assert!(message.validate().is_err());
    }

    #[test]
    fn validate_rejects_oversized_instruction_account_list() {
        let mut message = valid_message();
        message.instructions[0].accounts = vec![0; usize::from(u8::MAX) + 1];

        assert!(message.validate().is_err());
    }
}
