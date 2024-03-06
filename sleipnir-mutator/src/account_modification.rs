use std::str::FromStr;

use sleipnir_program::sleipnir_instruction;
use solana_sdk::{account::Account, commitment_config::CommitmentLevel, pubkey::Pubkey};

use crate::errors::MutatorResult;

#[derive(Default, Debug)]
pub struct AccountModification {
    pub account_address: String,
    pub lamports: Option<u64>,
    pub owner: Option<String>,
    pub executable: Option<bool>,
    pub data: Option<Vec<u8>>,
    pub rent_epoch: Option<u64>,
}

impl From<(&Account, &str)> for AccountModification {
    fn from((account, address): (&Account, &str)) -> Self {
        Self {
            account_address: address.to_string(),
            lamports: Some(account.lamports),
            owner: Some(account.owner.to_string()),
            executable: Some(account.executable),
            data: Some(account.data.clone()),
            rent_epoch: Some(account.rent_epoch),
        }
    }
}

impl AccountModification {
    pub fn try_into_sleipnir_program_account_modification(
        self,
    ) -> MutatorResult<(Pubkey, sleipnir_instruction::AccountModification)> {
        let pubkey = Pubkey::from_str(&self.account_address)?;
        let owner = self
            .owner
            .as_ref()
            .map(|o| Pubkey::from_str(o))
            .transpose()?;
        Ok((
            pubkey,
            sleipnir_instruction::AccountModification {
                lamports: self.lamports,
                owner,
                executable: self.executable,
                data: self.data,
                rent_epoch: self.rent_epoch,
            },
        ))
    }
}

// -----------------
// ModifyAccountOpts
// -----------------
pub struct ModifyAccountOpts {
    /// Commitment level to use for the transaction when waiting for transaction to be
    /// confirmed.
    pub commitment: CommitmentLevel,
}

impl Default for ModifyAccountOpts {
    fn default() -> Self {
        Self {
            commitment: CommitmentLevel::Confirmed,
        }
    }
}

// -----------------
// CloneAccountOpts
// -----------------
pub struct CloneAccountOpts {
    pub commitment: CommitmentLevel,
}

impl From<&ModifyAccountOpts> for CloneAccountOpts {
    fn from(opts: &ModifyAccountOpts) -> Self {
        Self {
            commitment: opts.commitment,
        }
    }
}

impl Default for CloneAccountOpts {
    fn default() -> Self {
        Self {
            commitment: CommitmentLevel::Confirmed,
        }
    }
}

// -----------------
// RestoreSnapshotAccountsOpts
// -----------------
pub struct RestoreSnapshotAccountsOpts {
    pub commitment: CommitmentLevel,
}

impl Default for RestoreSnapshotAccountsOpts {
    fn default() -> Self {
        Self {
            commitment: CommitmentLevel::Confirmed,
        }
    }
}
