use serde::{Deserialize, Serialize};
use solana_account::{Account, AccountSharedData};
use solana_message::Address as Pubkey;
use wincode::{SchemaRead, SchemaWrite};

use crate::token_programs::try_remap_ata_to_eata;

pub type CommittedAccountRef = (Pubkey, AccountSharedData);

#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, SchemaRead, SchemaWrite,
)]
pub struct CommittedAccount {
    pub pubkey: Pubkey,
    pub account: Account,
    pub remote_slot: u64,
}

impl From<CommittedAccountRef> for CommittedAccount {
    fn from(value: CommittedAccountRef) -> Self {
        let remote_slot = value.1.slot();
        Self {
            pubkey: value.0,
            account: value.1.into(),
            remote_slot,
        }
    }
}

impl CommittedAccount {
    /// Build a CommittedAccount from an AccountSharedData reference, optionally
    /// overriding the owner with `parent_program_id` and remapping ATA -> eATA
    /// if applicable.
    pub fn from_account_shared(
        pubkey: Pubkey,
        account_shared: &AccountSharedData,
        parent_program_id: Option<Pubkey>,
    ) -> Self {
        let remote_slot = account_shared.slot();
        if let Some((eata_pubkey, eata)) =
            try_remap_ata_to_eata(&pubkey, account_shared)
        {
            return CommittedAccount {
                pubkey: eata_pubkey,
                account: eata.into(),
                remote_slot,
            };
        }

        let mut account: Account = account_shared.to_owned().into();
        account.owner = parent_program_id.unwrap_or(account.owner);

        CommittedAccount {
            pubkey,
            account,
            remote_slot,
        }
    }
}
