//! Remote subscription identities shared by live transports.

use solana_loader_v3_interface::get_program_data_address;
use solana_pubkey::Pubkey;

/// Remote address watched by a transport and optional Loader V3 program to update.
///
/// Ordinary subscriptions watch and update the same address. Loader V3 subscriptions
/// watch ProgramData but materialize its normalized ELF at the local program address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AccountSubscription {
    pub(super) pubkey: Pubkey,
    /// Program address to materialize ProgramData ELF under; `None` updates `pubkey` itself.
    pub(super) program: Option<Pubkey>,
}

impl AccountSubscription {
    pub(super) fn local_pubkey(self) -> Pubkey {
        self.program.unwrap_or(self.pubkey)
    }

    /// Watches the derived ProgramData address while materializing updates under the program address.
    pub(super) fn program_data(pubkey: Pubkey) -> Self {
        Self {
            pubkey: get_program_data_address(&pubkey),
            program: Some(pubkey),
        }
    }

    /// Returns the subscriptions that can update this local address:
    /// the address itself and its derived ProgramData address.
    pub(super) fn for_account(pubkey: Pubkey) -> [Self; 2] {
        [Self { pubkey, program: None }, Self::program_data(pubkey)]
    }
}
