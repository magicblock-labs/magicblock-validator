use borsh::BorshDeserialize;
use compressed_delegation_api::{
    CommitAndFinalizeArgs, CompressedDelegationRecord,
};
use magicblock_core::intent::CommittedAccount;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::{
    intent_executor::task_info_fetcher::CompressedData, tasks::BaseTaskImpl,
};

/// A task that commits a delegated account's state to the base layer and finalizes it in the same
/// instruction, using compressed data.
///
/// The delivery strategy ([`CommitDelivery`]) determines how the data reaches
/// the chain (inline args vs buffer, full state vs diff).
#[derive(Clone, Debug)]
pub struct CommitFinalizeCompressedTask {
    pub commit_id: u64,
    pub allow_undelegation: bool,
    pub committed_account: CommittedAccount,
    pub compressed_data: CompressedData,
}

impl CommitFinalizeCompressedTask {
    #[inline(always)]
    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        let old_record = CompressedDelegationRecord::try_from_slice(
            &self.compressed_data.compressed_delegation_record_bytes,
        ).expect("The record should have been valid because it was already used to clone the account");
        let new_record = CompressedDelegationRecord {
            pda: self.committed_account.pubkey,
            authority: *validator,
            last_update_nonce: self.commit_id + 1,
            is_undelegatable: self.allow_undelegation,
            owner: old_record.owner,
            delegation_slot: old_record.delegation_slot,
            lamports: self.committed_account.account.lamports,
            data: self.committed_account.account.data.clone(),
        };
        let args = CommitAndFinalizeArgs {
            current_compressed_delegated_account_data: self
                .compressed_data
                .compressed_delegation_record_bytes
                .clone(),
            new_data: borsh::to_vec(&new_record)
                .expect("The serializing the new record should not fail"),
            account_meta: self.compressed_data.account_meta,
            validity_proof: self.compressed_data.proof,
            update_nonce: self.commit_id,
            allow_undelegation: self.allow_undelegation,
        };

        compressed_delegation_client::builders::CommitAndFinalizeBuilder {
            validator: *validator,
            delegated_account: self.committed_account.pubkey,
            remaining_accounts: self.compressed_data.remaining_accounts.clone(),
            args,
        }
        .instruction()
        .expect("The serializing the args should not fail")
    }
}

impl From<CommitFinalizeCompressedTask> for BaseTaskImpl {
    fn from(value: CommitFinalizeCompressedTask) -> Self {
        Self::CommitFinalizeCompressed(value)
    }
}
