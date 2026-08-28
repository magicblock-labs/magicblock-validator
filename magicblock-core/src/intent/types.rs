use serde::{Deserialize, Serialize};
use solana_account::{Account, AccountSharedData, ReadableAccount};
use solana_message::Address as Pubkey;
use wincode::{SchemaRead, SchemaWrite};

use crate::token_programs::try_remap_ata_to_eata;

/// Engine-internal wrapper that runs post-delegation actions. It is never the
/// owner program of a committed user account; treating it as one makes DLP
/// finalize reject the validator (`InvalidAuthority`).
const MAGIC_ROOT_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("MagicRootDRJ5atQjSJUxFjXzjeZXMADHUDznbk22gy");

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

        let mut account = Account {
            lamports: account_shared.lamports(),
            data: account_shared.data().to_vec(),
            owner: *account_shared.owner(),
            executable: account_shared.executable(),
            rent_epoch: account_shared.rent_epoch(),
        };
        // Rescue undelegation is CPI'd from MagicRoot. Keep the ER owner
        // (the user program) so L1 finalize restores the same owner.
        if let Some(parent) = parent_program_id {
            if parent != MAGIC_ROOT_PROGRAM_ID {
                account.owner = parent;
            }
        }

        CommittedAccount {
            pubkey,
            account,
            remote_slot,
        }
    }
}

#[cfg(test)]
mod tests {
    use solana_account::AccountSharedData;
    use solana_pubkey::Pubkey as SolanaPubkey;

    use super::*;

    #[test]
    fn rescue_undelegate_keeps_user_program_owner() {
        let owner = SolanaPubkey::new_unique();
        let account = AccountSharedData::new(1_000, 8, &owner);
        let committed = CommittedAccount::from_account_shared(
            SolanaPubkey::new_unique(),
            &account,
            Some(MAGIC_ROOT_PROGRAM_ID),
        );
        assert_eq!(committed.account.owner, owner);
    }

    #[test]
    fn parent_program_still_overrides_owner_when_not_magic_root() {
        let owner = SolanaPubkey::new_unique();
        let parent = SolanaPubkey::new_unique();
        let account = AccountSharedData::new(1_000, 8, &owner);
        let committed = CommittedAccount::from_account_shared(
            SolanaPubkey::new_unique(),
            &account,
            Some(parent),
        );
        assert_eq!(committed.account.owner, parent);
    }
}
