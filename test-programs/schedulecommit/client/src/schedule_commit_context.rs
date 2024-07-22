use std::{str::FromStr, thread::sleep, time::Duration};

use anyhow::{Context, Result};
use schedulecommit_program::api::{init_account_instruction, pda_with_bump};
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::{
    RpcSendTransactionConfig, RpcTransactionConfig,
};
use solana_sdk::{
    commitment_config::CommitmentConfig,
    hash::Hash,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::{SeedDerivable, Signer},
    transaction::Transaction,
};

pub struct ScheduleCommitTestContext {
    pub payer: Keypair,
    pub committees: Vec<Pubkey>,
    pub commitment: CommitmentConfig,
    pub client: RpcClient,
    pub validator_identity: Pubkey,
    pub blockhash: Hash,
}

impl Default for ScheduleCommitTestContext {
    fn default() -> Self {
        Self::new(1)
    }
}

impl ScheduleCommitTestContext {
    pub fn new(ncommittees: usize) -> Self {
        let payer = Keypair::from_seed(&[2u8; 32]).unwrap();
        // Create a new keypairs for the committees
        let committees = (0..ncommittees)
            .map(|idx| pda_with_bump(&payer.pubkey(), &[idx as u8]))
            .collect::<Vec<Pubkey>>();

        let commitment = CommitmentConfig::confirmed();

        let client = RpcClient::new_with_commitment(
            "http://localhost:8899".to_string(),
            commitment,
        );
        client
            .request_airdrop(&payer.pubkey(), LAMPORTS_PER_SOL * 100)
            .unwrap();
        let validator_identity = client.get_identity().unwrap();
        let blockhash = client.get_latest_blockhash().unwrap();

        Self {
            payer,
            committees,
            commitment,
            client,
            blockhash,
            validator_identity,
        }
    }

    pub fn init_committes(&self) -> Result<Signature> {
        let ixs = self
            .committees
            .iter()
            .enumerate()
            .map(|(idx, committee)| {
                init_account_instruction(
                    self.payer.pubkey(),
                    *committee,
                    idx as u8,
                )
            })
            .collect::<Vec<_>>();

        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&self.payer.pubkey()),
            &[&self.payer],
            self.blockhash,
        );
        self.client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                self.commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .with_context(|| "Failed to initialize committees")
    }

    pub fn confirm_transaction(
        &self,
        sig: &Signature,
        rpc_client: Option<&RpcClient>,
    ) -> Result<bool, String> {
        // Wait for the transaction to be confirmed (up to 1 sec)
        let mut count = 0;
        loop {
            match rpc_client
                .unwrap_or(&self.client)
                .confirm_transaction_with_commitment(sig, self.commitment)
            {
                Ok(res) => {
                    return Ok(res.value);
                }
                Err(err) => {
                    count += 1;
                    if count >= 5 {
                        return Err(format!("{:#?}", err));
                    } else {
                        sleep(Duration::from_millis(200));
                    }
                }
            }
        }
    }

    pub fn fetch_logs(
        &self,
        sig: Signature,
        rpc_client: Option<&RpcClient>,
    ) -> Option<Vec<String>> {
        // Try this up to 10 times since devnet here returns the version response instead of
        // the EncodedConfirmedTransactionWithStatusMeta at times
        for _ in 0..10 {
            let status = match rpc_client
                .unwrap_or(&self.client)
                .get_transaction_with_config(
                    &sig,
                    RpcTransactionConfig {
                        commitment: Some(self.commitment),
                        ..Default::default()
                    },
                ) {
                Ok(status) => status,
                Err(_) => {
                    sleep(Duration::from_millis(400));
                    continue;
                }
            };
            return Option::<Vec<String>>::from(
                status
                    .transaction
                    .meta
                    .as_ref()
                    .unwrap()
                    .log_messages
                    .clone(),
            );
        }
        None
    }

    pub fn extract_chain_transaction_signature(
        &self,
        logs: &[String],
    ) -> Option<Signature> {
        for log in logs {
            if log.starts_with("CommitTransactionSignature: ") {
                let commit_sig =
                    log.split_whitespace().last().expect("No signature found");
                return Signature::from_str(commit_sig).ok();
            }
        }
        None
    }
}
