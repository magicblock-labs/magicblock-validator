use std::path::Path;

use solana_pubkey::pubkey;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};

// mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev
const TEST_KEYPAIR_BYTES: [u8; 64] = [
    7, 83, 184, 55, 200, 223, 238, 137, 166, 244, 107, 126, 189, 16, 194, 36,
    228, 68, 43, 143, 13, 91, 3, 81, 53, 253, 26, 36, 50, 198, 40, 159, 11, 80,
    9, 208, 183, 189, 108, 200, 89, 77, 168, 76, 233, 197, 132, 22, 21, 186,
    202, 240, 105, 168, 157, 64, 233, 249, 100, 104, 210, 41, 83, 87,
];
// tEsT3eV6RFCWs1BZ7AXTzasHqTtMnMLCB2tjQ42TDXD
// 62LxqpAW6SWhp7iKBjCQneapn1w6btAhW7xHeREWSpPzw3xZbHCfAFesSR4R76ejQXCLWrndn37cKCCLFvx6Swps
pub const DLP_TEST_AUTHORITY_BYTES: [u8; 64] = [
    251, 62, 129, 184, 107, 49, 62, 184, 1, 147, 178, 128, 185, 157, 247, 92,
    56, 158, 145, 53, 51, 226, 202, 96, 178, 248, 195, 133, 133, 237, 237, 146,
    13, 32, 77, 204, 244, 56, 166, 172, 66, 113, 150, 218, 112, 42, 110, 181,
    98, 158, 222, 194, 130, 93, 175, 100, 190, 106, 9, 69, 156, 80, 96, 72,
];

pub struct LoadedAccounts {
    validator_authority_kp: Keypair,
    luzid_authority: Pubkey,
    extra_accounts: Vec<(String, String)>,
}

impl Default for LoadedAccounts {
    fn default() -> Self {
        Self {
            validator_authority_kp: Keypair::from_bytes(&TEST_KEYPAIR_BYTES)
                .expect("Failed to create validator authority keypair"),
            luzid_authority: pubkey!(
                "LUzidNSiPNjYNkxZcUm5hYHwnWPwsUfh2US1cpWwaBm"
            ),
            extra_accounts: vec![],
        }
    }
}

impl LoadedAccounts {
    pub fn new_with_new_validator_authority() -> Self {
        Self {
            validator_authority_kp: Keypair::new(),
            luzid_authority: pubkey!(
                "LUzidNSiPNjYNkxZcUm5hYHwnWPwsUfh2US1cpWwaBm"
            ),
            extra_accounts: vec![],
        }
    }

    /// This use the test authority used in the delegation program as the validator
    /// authority.
    /// https://github.com/magicblock-labs/delegation-program/blob/7fc0ae9a59e48bea5b046b173ea0e34fd433c3c7/tests/fixtures/accounts.rs#L46
    /// It is compiled in as the authority for the validator vault when we build
    /// the delegation program via:
    /// `cargo build-sbf --features=unit_test_config`
    pub fn with_delegation_program_test_authority() -> Self {
        Self {
            validator_authority_kp: Keypair::from_bytes(
                &DLP_TEST_AUTHORITY_BYTES,
            )
            .expect("Failed to create validator authority keypair"),
            luzid_authority: pubkey!(
                "LUzidNSiPNjYNkxZcUm5hYHwnWPwsUfh2US1cpWwaBm"
            ),
            extra_accounts: vec![],
        }
    }

    pub fn validator_authority_keypair(&self) -> &Keypair {
        &self.validator_authority_kp
    }

    pub fn validator_authority_base58(&self) -> String {
        self.validator_authority_kp.to_base58_string()
    }

    pub fn validator_authority(&self) -> Pubkey {
        self.validator_authority_kp.pubkey()
    }

    pub fn luzid_authority(&self) -> Pubkey {
        self.luzid_authority
    }

    pub fn validator_fees_vault(&self) -> Pubkey {
        dlp::pda::validator_fees_vault_pda_from_validator(
            &self.validator_authority(),
        )
    }
    pub fn protocol_fees_vault(&self) -> Pubkey {
        dlp::pda::fees_vault_pda()
    }
    pub fn extra_accounts(
        &self,
        workspace_dir: &Path,
        accounts_dir: &Path,
    ) -> Vec<(String, String)> {
        self.extra_accounts
            .iter()
            .map(|(k, v)| {
                // Either we have a relative path to the root dir or
                // just a filename of an account in the accounts dir
                let path = if v.contains("/") {
                    workspace_dir.join(v)
                } else {
                    accounts_dir.join(v)
                };
                (k.clone(), path.to_string_lossy().to_string())
            })
            .collect::<Vec<_>>()
    }

    pub fn add(&mut self, accounts: &[(&str, &str)]) {
        for (pubkey, filename) in accounts {
            self.extra_accounts
                .push((pubkey.to_string(), filename.to_string()));
        }
    }
}
