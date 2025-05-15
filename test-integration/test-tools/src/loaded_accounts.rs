use solana_pubkey::pubkey;
use solana_sdk::pubkey::Pubkey;

pub struct LoadedAccounts {
    validator_authority: Pubkey,
    luzid_authority: Pubkey,
}

impl Default for LoadedAccounts {
    fn default() -> Self {
        Self {
            validator_authority: pubkey!(
                "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev"
            ),
            luzid_authority: pubkey!(
                "LUzidNSiPNjYNkxZcUm5hYHwnWPwsUfh2US1cpWwaBm"
            ),
        }
    }
}

impl LoadedAccounts {
    pub fn new(validator_authority: Pubkey, luzid_authority: Pubkey) -> Self {
        Self {
            validator_authority,
            luzid_authority,
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
            validator_authority: pubkey!(
                "tEsT3eV6RFCWs1BZ7AXTzasHqTtMnMLCB2tjQ42TDXD"
            ),
            luzid_authority: pubkey!(
                "LUzidNSiPNjYNkxZcUm5hYHwnWPwsUfh2US1cpWwaBm"
            ),
        }
    }

    pub fn validator_authority(&self) -> Pubkey {
        self.validator_authority
    }

    pub fn luzid_authority(&self) -> Pubkey {
        self.luzid_authority
    }

    pub fn validator_fees_vault(&self) -> Pubkey {
        dlp::pda::validator_fees_vault_pda_from_validator(
            &self.validator_authority,
        )
    }
    pub fn protocol_fees_vault(&self) -> Pubkey {
        dlp::pda::fees_vault_pda()
    }
}
