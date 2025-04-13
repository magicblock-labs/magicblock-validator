use std::io;

use anyhow::Context;
use borsh::BorshDeserialize;
use log::info;
use mdp::{
    consts::ER_RECORD_SEED,
    instructions::{sync::SyncInstruction, version::v0::SyncRecordV0},
    state::record::ErRecord,
    ID,
};
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    account::ReadableAccount,
    commitment_config::CommitmentConfig,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

pub struct DomainRegistryManager {
    client: RpcClient,
}

impl DomainRegistryManager {
    pub fn new(url: impl ToString) -> Self {
        Self {
            client: RpcClient::new_with_commitment(
                url.to_string(),
                CommitmentConfig::confirmed(),
            ),
        }
    }

    pub fn fetch_validator_info(
        &self,
        account_pubkey: &Pubkey,
    ) -> Result<Option<ErRecord>, Error> {
        let response = self
            .client
            .get_account_with_commitment(
                account_pubkey,
                CommitmentConfig::confirmed(),
            )
            .context(format!(
                "Failed to get account: {} from server: {}",
                account_pubkey,
                self.client.url()
            ))?;

        response
            .value
            .map(|account| {
                let mut data = account.data();
                ErRecord::deserialize(&mut data).map_err(Error::BorshError)
            })
            .transpose()
    }

    fn register(
        &self,
        payer: &Keypair,
        validator_info: ErRecord,
    ) -> Result<(), Error> {
        let (pda, _) = validator_info.pda();
        self.send_instruction(
            payer,
            pda,
            mdp::instructions::Instruction::Register(validator_info),
        )
        .context("Failed to send register tx")?;

        Ok(())
    }

    pub fn sync(
        &self,
        payer: &Keypair,
        validator_info: &ErRecord,
    ) -> Result<(), Error> {
        let sync_info = SyncRecordV0 {
            identity: *validator_info.identity(),
            status: Some(validator_info.status()),
            block_time_ms: Some(validator_info.block_time_ms()),
            base_fee: Some(validator_info.base_fee()),
            features: Some(validator_info.features().clone()),
            load_average: Some(validator_info.load_average()),
            country_code: Some(validator_info.country_code()),
            addr: Some(validator_info.addr().to_owned()),
        };

        let (pda, _) = validator_info.pda();
        self.send_instruction(
            payer,
            pda,
            mdp::instructions::Instruction::Sync(SyncInstruction::V0(
                sync_info,
            )),
        )
        .context("Could not send sync transaction")?;

        Ok(())
    }

    pub fn get_pda(pubkey: &Pubkey) -> (Pubkey, u8) {
        let seeds = [ER_RECORD_SEED, pubkey.as_ref()];
        Pubkey::find_program_address(&seeds, &ID)
    }

    pub fn unregister(&self, payer: &Keypair) -> Result<(), Error> {
        let (pda, _) = Self::get_pda(&payer.pubkey());

        // Verify existence to avoid failed tx costs
        let _ = self
            .fetch_validator_info(&pda)?
            .ok_or(Error::NoRegisteredValidatorError)?;
        self.send_instruction(
            payer,
            pda,
            mdp::instructions::Instruction::Unregister(payer.pubkey()),
        )
        .context("Failed to unregister")?;

        Ok(())
    }

    pub fn handle_registration(
        &self,
        payer: &Keypair,
        validator_info: ErRecord,
    ) -> Result<(), Error> {
        match self.fetch_validator_info(&validator_info.pda().0)? {
            Some(current_validator_info) => {
                if current_validator_info == validator_info {
                    info!("Domain registry record for the validator is up to date, skipping sync");
                    Ok(())
                } else {
                    info!("Domain registry record for the validator requires update, syncing data");
                    self.sync(payer, &validator_info)
                }
            }
            None => {
                info!("Domain registry record for the validator absent, registering");
                self.register(payer, validator_info)
            }
        }
    }

    pub fn handle_registration_static(
        url: impl ToString,
        payer: &Keypair,
        validator_info: ErRecord,
    ) -> Result<(), Error> {
        let manager = DomainRegistryManager::new(url);
        manager.handle_registration(payer, validator_info)
    }

    fn send_instruction<T: borsh::BorshSerialize>(
        &self,
        payer: &Keypair,
        pda: Pubkey,
        instruction: T,
    ) -> Result<(), anyhow::Error> {
        let accounts = vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(pda, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ];

        let instruction =
            Instruction::new_with_borsh(ID, &instruction, accounts);
        let recent_blockhash = self
            .client
            .get_latest_blockhash()
            .map_err(anyhow::Error::from)?;
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&payer.pubkey()),
            &[&payer],
            recent_blockhash,
        );

        self.client
            .send_and_confirm_transaction(&transaction)
            .map_err(anyhow::Error::from)?;
        Ok(())
    }

    pub fn handle_unregistration_static(
        url: impl ToString,
        payer: &Keypair,
    ) -> Result<(), Error> {
        info!("Unregistering validator's record from domain registry");
        let manager = DomainRegistryManager::new(url);
        manager.unregister(payer)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("BorshError: {0}")]
    BorshError(#[from] io::Error),
    #[error("No validator to unregister")]
    NoRegisteredValidatorError,
    #[error("UnknownError: {0}")]
    UnknownError(#[from] anyhow::Error),
}
