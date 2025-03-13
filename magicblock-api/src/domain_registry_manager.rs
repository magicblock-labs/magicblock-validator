use std::io;

use anyhow::Context;
use borsh::BorshDeserialize;
use log::info;
use mdp::{
    consts::VALIDATOR_INFO_SEED,
    instructions::{
        register::RegisterInstruction, sync::SyncInfoInstruction,
        unregister::UnregisterInstruction,
    },
    state::validator_info::ValidatorInfo,
    ID,
};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
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
    const ACCOUNT_NOT_FOUND_FILTER: &'static str = "AccountNotFound";

    pub fn new(url: impl ToString) -> Self {
        Self {
            client: RpcClient::new_with_commitment(
                url.to_string(),
                CommitmentConfig::confirmed(),
            ),
        }
    }

    pub async fn fetch_validator_info(
        &self,
        account_pubkey: &Pubkey,
    ) -> Result<Option<ValidatorInfo>, Error> {
        match self.client.get_account(account_pubkey).await {
            Ok(account) => {
                let mut data = account.data();
                let validator_info = ValidatorInfo::deserialize(&mut data)?;

                Ok(Some(validator_info))
            }
            Err(err) => {
                if err.to_string().contains(Self::ACCOUNT_NOT_FOUND_FILTER) {
                    Ok(None)
                } else {
                    Err(Error::UnknownError(anyhow::Error::from(err)))
                }
            }
        }
    }

    async fn register(
        &self,
        payer: &Keypair,
        validator_info: ValidatorInfo,
    ) -> Result<(), Error> {
        let (pda, _) = validator_info.pda();
        self.send_instruction(
            payer,
            pda,
            mdp::instructions::Instruction::Register(RegisterInstruction(
                validator_info,
            )),
        )
        .await
        .context("Failed to send register tx")?;

        Ok(())
    }

    pub async fn sync(
        &self,
        payer: &Keypair,
        validator_info: &ValidatorInfo,
    ) -> Result<(), Error> {
        let sync_info = SyncInfoInstruction {
            identity: validator_info.identity,
            addr: Some(validator_info.addr),
            block_time_ms: Some(validator_info.block_time_ms),
            fees: Some(validator_info.fees),
            features: Some(validator_info.features.clone()),
        };

        let (pda, _) = validator_info.pda();
        self.send_instruction(
            payer,
            pda,
            mdp::instructions::Instruction::SyncInfo(sync_info),
        )
        .await
        .context("Could not send sync transaction")?;

        Ok(())
    }

    pub fn get_pda(pubkey: &Pubkey) -> (Pubkey, u8) {
        let seeds: &[&[u8]] = &[VALIDATOR_INFO_SEED, pubkey.as_ref()];
        Pubkey::find_program_address(seeds, &ID)
    }

    pub async fn unregister(&self, payer: &Keypair) -> Result<(), Error> {
        let (pda, _) = Self::get_pda(&payer.pubkey());
        let unregister = UnregisterInstruction(payer.pubkey());
        self.send_instruction(
            payer,
            pda,
            mdp::instructions::Instruction::Unregister(unregister),
        )
        .await
        .context("Failed to unregister")?;

        Ok(())
    }

    pub async fn handle_registration(
        &self,
        payer: &Keypair,
        validator_info: ValidatorInfo,
    ) -> Result<(), Error> {
        match self.fetch_validator_info(&validator_info.pda().0).await? {
            Some(current_validator_info) => {
                if current_validator_info == validator_info {
                    info!("Data up to date, no need to sync");
                    Ok(())
                } else {
                    info!("Syncing data...");
                    self.sync(payer, &validator_info).await
                }
            }
            None => {
                info!("Registering...");
                self.register(payer, validator_info).await
            }
        }
    }

    pub async fn handle_registration_static(
        url: impl ToString,
        payer: &Keypair,
        validator_info: ValidatorInfo,
    ) -> Result<(), Error> {
        let manager = DomainRegistryManager::new(url);
        manager.handle_registration(payer, validator_info).await
    }

    async fn send_instruction<T: borsh::BorshSerialize>(
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
            .await
            .map_err(anyhow::Error::from)?;
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&payer.pubkey()),
            &[&payer],
            recent_blockhash,
        );

        self.client
            .send_and_confirm_transaction(&transaction)
            .await
            .map_err(anyhow::Error::from)?;
        Ok(())
    }

    pub async fn handle_unregistration_static(
        url: impl ToString,
        payer: &Keypair,
    ) -> Result<(), Error> {
        info!("Unregistering...");
        let manager = DomainRegistryManager::new(url);
        manager.unregister(payer).await
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("BorshError: {0}")]
    BorshError(#[from] io::Error),
    #[error("UnknownError: {0}")]
    UnknownError(#[from] anyhow::Error),
}
