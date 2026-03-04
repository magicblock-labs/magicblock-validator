use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use solana_account::{Account, AccountSharedData};
use solana_pubkey::Pubkey;

use crate::token_programs::try_remap_ata_to_eata;

pub type CommittedAccountRef<'a> = (Pubkey, &'a RefCell<AccountSharedData>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedAccount {
    pub pubkey: Pubkey,
    pub account: Account,
    pub remote_slot: u64,
}

impl<'a> From<CommittedAccountRef<'a>> for CommittedAccount {
    fn from(value: CommittedAccountRef<'a>) -> Self {
        let account = value.1.borrow();
        let remote_slot = account.remote_slot();
        Self {
            pubkey: value.0,
            account: account.to_owned().into(),
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
        let remote_slot = account_shared.remote_slot();
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
