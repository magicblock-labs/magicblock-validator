use std::mem::size_of;

use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{
    compiled_instruction::CompiledInstruction, v0, CompileError, MessageHeader,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::{Signer, SignerError};

const MESSAGE_VERSION_PREFIX: u8 = 0x80;
const V1_PREFIX: u8 = MESSAGE_VERSION_PREFIX | 1;
const MAX_ADDRESSES: usize = 64;
const MAX_INSTRUCTIONS: usize = 64;
const MAX_SIGNATURES: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    header: MessageHeader,
    account_keys: Vec<Pubkey>,
    recent_blockhash: Hash,
    instructions: Vec<CompiledInstruction>,
}

impl Message {
    pub(crate) fn try_compile(
        payer: &Pubkey,
        instructions: &[Instruction],
        recent_blockhash: Hash,
    ) -> Result<Self, CompileError> {
        let message = v0::Message::try_compile(
            payer,
            instructions,
            &[],
            recent_blockhash,
        )?;
        Ok(Self {
            header: message.header,
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
        if self
            .instructions
            .iter()
            .any(|ix| ix.data.len() > u16::MAX as usize)
        {
            return Err(());
        }
        Ok(())
    }

    fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.serialized_size());
        out.push(V1_PREFIX);
        out.push(self.header.num_required_signatures);
        out.push(self.header.num_readonly_signed_accounts);
        out.push(self.header.num_readonly_unsigned_accounts);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(self.recent_blockhash.as_ref());
        out.push(self.instructions.len() as u8);
        out.push(self.account_keys.len() as u8);
        for key in &self.account_keys {
            out.extend_from_slice(key.as_ref());
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

    pub(crate) fn signature(&self) -> Signature {
        self.signature
    }

    pub(crate) fn recent_blockhash(&self) -> &Hash {
        &self.recent_blockhash
    }

    pub(crate) fn serialized(&self) -> &[u8] {
        &self.serialized
    }

    pub(crate) fn serialized_size(&self) -> usize {
        self.serialized.len()
    }
}
