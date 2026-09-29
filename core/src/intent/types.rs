use serde::{Deserialize, Serialize};
use solana_account::{Account, AccountSharedData, ReadableAccount};
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
    /// Build a CommittedAccount from an AccountSharedData reference, remapping
    /// ATA -> eATA if applicable.
    pub fn from_account_shared(
        pubkey: Pubkey,
        account_shared: &AccountSharedData,
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

        let account = Account {
            lamports: account_shared.lamports(),
            data: account_shared.data().to_vec(),
            owner: *account_shared.owner(),
            executable: account_shared.executable(),
            rent_epoch: account_shared.rent_epoch(),
        };
        CommittedAccount {
            pubkey,
            account,
            remote_slot,
        }
    }
}

#[cfg(test)]
mod tests {
    use solana_account::{AccountSharedData, WritableAccount};

    use super::*;

    /// Proves commit serialization retains account-state ownership without
    /// substituting invocation-frame provenance.
    #[test]
    fn committed_account_preserves_account_owner() {
        let owner = Pubkey::new_unique();
        let mut account = AccountSharedData::default();
        account.set_owner(owner);

        let committed = CommittedAccount::from_account_shared(
            Pubkey::new_unique(),
            &account,
        );

        assert_eq!(committed.account.owner, owner);
    }
}
