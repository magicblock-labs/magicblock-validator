use std::{str::FromStr, thread::sleep, time::Duration};

use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    hash::Hash,
    native_token::LAMPORTS_PER_SOL,
    signature::{Keypair, Signature},
    signer::{SeedDerivable, Signer},
};

pub struct TriggerCommitTestContext {
    pub payer: Keypair,
    pub committee: Keypair,
    pub commitment: CommitmentConfig,
    pub client: RpcClient,
    pub blockhash: Hash,
}

impl Default for TriggerCommitTestContext {
    fn default() -> Self {
        Self::new()
    }
}

impl TriggerCommitTestContext {
    pub fn new() -> Self {
        let payer = Keypair::from_seed(&[2u8; 32]).unwrap();
        let committee = Keypair::new();
        let commitment = CommitmentConfig::confirmed();

        let client = RpcClient::new_with_commitment(
            "http://localhost:8899".to_string(),
            commitment,
        );
        client
            .request_airdrop(&payer.pubkey(), LAMPORTS_PER_SOL * 100)
            .unwrap();
        // Account needs to exist to be commitable
        client
            .request_airdrop(&committee.pubkey(), LAMPORTS_PER_SOL)
            .unwrap();

        let blockhash = client.get_latest_blockhash().unwrap();

        Self {
            payer,
            committee,
            commitment,
            client,
            blockhash,
        }
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
        // Try this up to 5 times since devnet here returns the version response instead of
        // the EncodedConfirmedTransactionWithStatusMeta at times
        for _ in 0..5 {
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
                    sleep(Duration::from_millis(200));
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
