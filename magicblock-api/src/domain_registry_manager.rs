use std::{io, time::Duration};

use anyhow::{anyhow, Context};
use borsh::BorshDeserialize;
use mdp::{
    consts::ER_RECORD_SEED,
    instructions::{sync::SyncInstruction, version::v0::SyncRecordV0},
    state::record::ErRecord,
    ID,
};
use solana_account::ReadableAccount;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk_ids::system_program;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tokio::time::sleep;
use tracing::info;

pub struct DomainRegistryManager {
    client: RpcClient,
}

impl DomainRegistryManager {
    pub fn new(url: impl ToString) -> Self {
        Self::new_with_commitment(url, CommitmentConfig::confirmed())
    }

    pub fn new_with_commitment(
        url: impl ToString,
        commitment: CommitmentConfig,
    ) -> Self {
        Self {
            client: RpcClient::new_with_commitment(url.to_string(), commitment),
        }
    }

    pub async fn fetch_validator_info(
        &self,
        account_pubkey: &Pubkey,
    ) -> Result<Option<ErRecord>, Error> {
        let response = self
            .client
            .get_account_with_commitment(
                account_pubkey,
                self.client.commitment(),
            )
            .await
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

    async fn register(
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
        .await
        .context("Failed to send register tx")?;

        Ok(())
    }

    pub async fn sync(
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
        .await
        .context("Could not send sync transaction")?;

        Ok(())
    }

    pub fn get_pda(pubkey: &Pubkey) -> (Pubkey, u8) {
        let seeds = [ER_RECORD_SEED, pubkey.as_ref()];
        Pubkey::find_program_address(&seeds, &ID)
    }

    pub async fn unregister(&self, payer: &Keypair) -> Result<(), Error> {
        let (pda, _) = Self::get_pda(&payer.pubkey());

        // Verify existence to avoid failed tx costs
        let _ = self
            .fetch_validator_info(&pda)
            .await?
            .ok_or(Error::NoRegisteredValidatorError)?;
        self.send_instruction(
            payer,
            pda,
            mdp::instructions::Instruction::Unregister(payer.pubkey()),
        )
        .await
        .context("Failed to unregister")?;

        Ok(())
    }

    async fn send_unregister(
        &self,
        payer: &Keypair,
    ) -> Result<Signature, Error> {
        let (pda, _) = Self::get_pda(&payer.pubkey());

        // Verify existence to avoid failed tx costs
        let _ = self
            .fetch_validator_info(&pda)
            .await?
            .ok_or(Error::NoRegisteredValidatorError)?;
        self.send_instruction_without_confirmation(
            payer,
            pda,
            mdp::instructions::Instruction::Unregister(payer.pubkey()),
        )
        .await
        .context("Failed to send unregister tx")
        .map_err(Error::UnknownError)
    }

    pub async fn handle_registration(
        &self,
        payer: &Keypair,
        validator_info: ErRecord,
    ) -> Result<(), Error> {
        match self.fetch_validator_info(&validator_info.pda().0).await? {
            Some(current_validator_info) => {
                if current_validator_info == validator_info {
                    info!("Domain registry record up to date");
                    Ok(())
                } else {
                    info!("Domain registry record requires update");
                    self.sync(payer, &validator_info).await
                }
            }
            None => {
                info!("Domain registry record absent, registering");
                self.register(payer, validator_info).await
            }
        }
    }

    pub async fn handle_registration_static(
        url: impl ToString,
        payer: &Keypair,
        validator_info: ErRecord,
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
        let transaction =
            self.build_transaction(payer, pda, instruction).await?;
        self.client
            .send_and_confirm_transaction(&transaction)
            .await
            .context("Failed to send and confirm transaction")?;
        Ok(())
    }

    async fn send_instruction_without_confirmation<T: borsh::BorshSerialize>(
        &self,
        payer: &Keypair,
        pda: Pubkey,
        instruction: T,
    ) -> Result<Signature, anyhow::Error> {
        let transaction =
            self.build_transaction(payer, pda, instruction).await?;
        self.client
            .send_transaction(&transaction)
            .await
            .context("Failed to send transaction")
    }

    async fn build_transaction<T: borsh::BorshSerialize>(
        &self,
        payer: &Keypair,
        pda: Pubkey,
        instruction: T,
    ) -> Result<Transaction, anyhow::Error> {
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
            .context("Failed to get latest blockhash")?;
        Ok(Transaction::new_signed_with_payer(
            &[instruction],
            Some(&payer.pubkey()),
            &[&payer],
            recent_blockhash,
        ))
    }

    pub async fn handle_unregistration_static(
        url: impl ToString,
        payer: &Keypair,
    ) -> Result<(), Error> {
        info!("Unregistering validator from domain registry");
        let manager = DomainRegistryManager::new_with_commitment(
            url,
            CommitmentConfig::confirmed(),
        );
        manager.unregister(payer).await
    }

    pub async fn send_unregistration_static(
        url: impl ToString,
        payer: &Keypair,
    ) -> Result<Signature, Error> {
        info!("Sending validator unregister transaction");
        let manager = DomainRegistryManager::new_with_commitment(
            url,
            CommitmentConfig::confirmed(),
        );
        manager.send_unregister(payer).await
    }

    pub async fn confirm_signature_static(
        url: impl ToString,
        signature: Signature,
    ) -> Result<(), Error> {
        let manager = DomainRegistryManager::new_with_commitment(
            url,
            CommitmentConfig::confirmed(),
        );
        manager.confirm_signature(&signature).await
    }

    async fn confirm_signature(
        &self,
        signature: &Signature,
    ) -> Result<(), Error> {
        loop {
            match self
                .client
                .get_signature_status_with_commitment(
                    signature,
                    self.client.commitment(),
                )
                .await
                .context("Failed to get signature status")?
            {
                Some(Ok(())) => return Ok(()),
                Some(Err(err)) => {
                    return Err(Error::UnknownError(anyhow!(
                        "Transaction failed: {err}"
                    )));
                }
                None => sleep(Duration::from_millis(500)).await,
            }
        }
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
