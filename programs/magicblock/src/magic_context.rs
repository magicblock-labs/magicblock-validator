use std::mem;

use magicblock_core::magic_program;
use serde::{Deserialize, Serialize};
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    clock::Slot,
    hash::Hash,
    pubkey::Pubkey,
    transaction::Transaction,
};

use crate::magic_schedule_action::{
    CommitType, CommittedAccountV2, MagicAction, ScheduledAction,
    ShortAccountMeta, UndelegateType,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeePayerAccount {
    pub pubkey: Pubkey,
    pub delegated_pda: Pubkey,
}

// Q: can user initiate actions on arbitrary accounts?
// No, then he could call any handler on any porgram
// Inititating transfer for himself
//
// Answer: No

// Q; can user call any program but using account that he owns?
// Far example, there could Transfer from that implements logix for transfer
// Here the fact that magicblock-program schedyled that call huarantess that user apporved this
//
// Answer: Yes

// user has multiple actions that he wants to perform on owned accounts
// he may schedule
// Those actions may have contraints: Undelegate can come only After Commit
// Commit can't come after undelegate

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MagicContext {
    pub scheduled_commits: Vec<ScheduledAction>,
}

impl MagicContext {
    pub const SIZE: usize = magic_program::MAGIC_CONTEXT_SIZE;
    pub const ZERO: [u8; Self::SIZE] = [0; Self::SIZE];
    pub(crate) fn deserialize(
        data: &AccountSharedData,
    ) -> Result<Self, bincode::Error> {
        if data.data().is_empty() {
            Ok(Self::default())
        } else {
            data.deserialize_data()
        }
    }

    pub(crate) fn add_scheduled_action(&mut self, action: ScheduledAction) {
        self.scheduled_commits.push(action);
    }

    pub(crate) fn take_scheduled_commits(&mut self) -> Vec<ScheduledAction> {
        mem::take(&mut self.scheduled_commits)
    }

    pub fn has_scheduled_commits(data: &[u8]) -> bool {
        // Currently we only store a vec of scheduled commits in the MagicContext
        // The first 8 bytes contain the length of the vec
        // This works even if the length is actually stored as a u32
        // since we zero out the entire context whenever we update the vec
        !is_zeroed(&data[0..8])
    }
}

fn is_zeroed(buf: &[u8]) -> bool {
    const VEC_SIZE_LEN: usize = 8;
    const ZEROS: [u8; VEC_SIZE_LEN] = [0; VEC_SIZE_LEN];
    let mut chunks = buf.chunks_exact(VEC_SIZE_LEN);

    #[allow(clippy::indexing_slicing)]
    {
        chunks.all(|chunk| chunk == &ZEROS[..])
            && chunks.remainder() == &ZEROS[..chunks.remainder().len()]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledCommit {
    pub id: u64,
    pub slot: Slot,
    pub blockhash: Hash,
    pub accounts: Vec<CommittedAccount>,
    pub payer: Pubkey,
    pub commit_sent_transaction: Transaction,
    pub request_undelegation: bool,
}

impl From<ScheduledCommit> for ScheduledAction {
    fn from(value: ScheduledCommit) -> Self {
        Self {
            id: value.id,
            slot: value.slot,
            blockhash: value.blockhash,
            payer: value.payer,
            action_sent_transaction: value.commit_sent_transaction,
            action: MagicAction::Commit(CommitType::Standalone(
                value
                    .accounts
                    .into_iter()
                    .map(CommittedAccountV2::from)
                    .collect(),
            )),
        }
    }
}

impl TryFrom<ScheduledAction> for ScheduledCommit {
    type Error = ();
    fn try_from(value: ScheduledAction) -> Result<Self, Self::Error> {
        fn extract_accounts(
            commit_type: CommitType,
        ) -> Result<Vec<CommittedAccount>, ()> {
            match commit_type {
                CommitType::Standalone(committed_accounts) => {
                    Ok(committed_accounts
                        .into_iter()
                        .map(CommittedAccount::from)
                        .collect())
                }
                CommitType::WithHandler { .. } => Err(()),
            }
        }

        let (accounts, request_undelegation) = match value.action {
            MagicAction::Commit(commit_action) => {
                let accounts = extract_accounts(commit_action)?;
                Ok((accounts, false))
            }
            MagicAction::CommitAndUndelegate(value) => {
                if let UndelegateType::Standalone = value.undelegate_action {
                    Ok(())
                } else {
                    Err(())
                }?;

                let accounts = extract_accounts(value.commit_action)?;
                Ok((accounts, true))
            }
            MagicAction::CallHandler(_) => Err(()),
        }?;

        Ok(Self {
            id: value.id,
            slot: value.slot,
            blockhash: value.blockhash,
            payer: value.payer,
            commit_sent_transaction: value.action_sent_transaction,
            accounts,
            request_undelegation,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedAccount {
    pub pubkey: Pubkey,
    // TODO(GabrielePicco): We should read the owner from the delegation record rather
    // than deriving/storing it. To remove once the cloning pipeline allow us to easily access the owner.
    pub owner: Pubkey,
}

impl From<CommittedAccount> for CommittedAccountV2 {
    fn from(value: CommittedAccount) -> Self {
        Self {
            owner: value.owner,
            short_meta: ShortAccountMeta {
                pubkey: value.pubkey,
                is_writable: false,
            },
        }
    }
}

impl From<CommittedAccountV2> for CommittedAccount {
    fn from(value: CommittedAccountV2) -> Self {
        Self {
            pubkey: value.short_meta.pubkey,
            owner: value.owner,
        }
    }
}
