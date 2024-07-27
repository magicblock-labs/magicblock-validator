use std::{str::FromStr, thread::sleep, time::Duration};

use anyhow::{Context, Result};
use schedulecommit_program::api::{
    delegate_account_cpi_instruction, init_account_instruction, pda_and_bump,
};
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::{
    client_error,
    config::{RpcSendTransactionConfig, RpcTransactionConfig},
};
#[allow(unused_imports)]
use solana_sdk::signer::SeedDerivable;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    hash::Hash,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

pub struct ScheduleCommitTestContext {
    // The first payer from the committees array which is used to fund transactions
    pub payer: Keypair,
    // The Payer keypairs along with its PDA pubkey which we'll commit
    pub committees: Vec<(Keypair, Pubkey)>,
    pub commitment: CommitmentConfig,
    pub chain_client: RpcClient,
    pub ephem_client: RpcClient,
    pub validator_identity: Pubkey,
    pub chain_blockhash: Hash,
    pub ephem_blockhash: Hash,
}

impl Default for ScheduleCommitTestContext {
    fn default() -> Self {
        Self::new(1)
    }
}

impl ScheduleCommitTestContext {
    // -----------------
    // Init
    // -----------------
    pub fn new(ncommittees: usize) -> Self {
        let commitment = CommitmentConfig::confirmed();

        let chain_client = RpcClient::new_with_commitment(
            "http://localhost:7799".to_string(),
            commitment,
        );
        let ephem_client = RpcClient::new_with_commitment(
            "http://localhost:8899".to_string(),
            commitment,
        );

        // Each committee is the payer and the matching PDA
        // The payer has money airdropped in order to init its PDA.
        // However in order to commit we can use any payer as the only
        // requirement is that the PDA is owned by its program.
        let committees = (0..ncommittees)
            .map(|_idx| {
                let payer = Keypair::from_seed(&[_idx as u8; 32]).unwrap();
                Self::airdrop(
                    &chain_client,
                    &payer.pubkey(),
                    LAMPORTS_PER_SOL,
                    commitment,
                )
                .unwrap();
                let (pda, _) = pda_and_bump(&payer.pubkey());
                (payer, pda)
            })
            .collect::<Vec<(Keypair, Pubkey)>>();

        let validator_identity = chain_client.get_identity().unwrap();
        let chain_blockhash = chain_client.get_latest_blockhash().unwrap();
        let ephem_blockhash = ephem_client.get_latest_blockhash().unwrap();

        let payer = committees[0].0.insecure_clone();
        Self {
            payer,
            committees,
            commitment,
            chain_client,
            ephem_client,
            chain_blockhash,
            ephem_blockhash,
            validator_identity,
        }
    }

    // -----------------
    // Schedule Commit specific Transactions
    // -----------------
    pub fn init_committees(&self) -> Result<Signature> {
        let ixs = self
            .committees
            .iter()
            .map(|(payer, committee)| {
                init_account_instruction(payer.pubkey(), *committee)
            })
            .collect::<Vec<_>>();

        let payers = self
            .committees
            .iter()
            .map(|(payer, _)| payer)
            .collect::<Vec<_>>();

        // The init tx for all payers is funded by the first payer for simplicity
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&payers[0].pubkey()),
            &payers,
            self.chain_blockhash,
        );
        self.chain_client
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

    pub fn delegate_committees(&self) -> Result<Signature> {
        let mut ixs = vec![];
        let mut payers = vec![];
        for (payer, _) in &self.committees {
            let ix = delegate_account_cpi_instruction(payer.pubkey());
            ixs.push(ix);
            payers.push(payer);
        }

        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&payers[0].pubkey()),
            &payers,
            self.chain_blockhash,
        );
        self.chain_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                self.commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .with_context(|| {
                format!(
                    "Failed to delegate committees '{:?}'",
                    tx.signatures[0]
                )
            })
    }

    // -----------------
    // Fetch Logs
    // -----------------
    pub fn fetch_ephemeral_logs(&self, sig: Signature) -> Option<Vec<String>> {
        self.fetch_logs(sig, Some(&self.ephem_client))
    }

    pub fn fetch_chain_logs(&self, sig: Signature) -> Option<Vec<String>> {
        self.fetch_logs(sig, Some(&self.chain_client))
    }

    fn fetch_logs(
        &self,
        sig: Signature,
        rpc_client: Option<&RpcClient>,
    ) -> Option<Vec<String>> {
        // Try this up to 10 times since devnet here returns the version response instead of
        // the EncodedConfirmedTransactionWithStatusMeta at times
        for _ in 0..10 {
            let status = match rpc_client
                .unwrap_or(&self.chain_client)
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

    // -----------------
    // Fetch Account Data/Balance
    // -----------------
    pub fn fetch_ephem_account_data(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<Vec<u8>> {
        self.ephem_client
            .get_account_data(&pubkey)
            .with_context(|| {
                format!(
                    "Failed to fetch ephemeral account data for '{:?}'",
                    pubkey
                )
            })
    }

    pub fn fetch_chain_account_data(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<Vec<u8>> {
        self.chain_client
            .get_account_data(&pubkey)
            .with_context(|| {
                format!("Failed to fetch chain account data for '{:?}'", pubkey)
            })
    }

    pub fn fetch_ephem_account_balance(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<u64> {
        self.ephem_client
            .get_balance_with_commitment(&pubkey, self.commitment)
            .map(|balance| balance.value)
            .with_context(|| {
                format!(
                    "Failed to fetch ephemeral account balance for '{:?}'",
                    pubkey
                )
            })
    }

    pub fn fetch_chain_account_balance(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<u64> {
        self.chain_client
            .get_balance_with_commitment(&pubkey, self.commitment)
            .map(|balance| balance.value)
            .with_context(|| {
                format!(
                    "Failed to fetch chain account balance for '{:?}'",
                    pubkey
                )
            })
    }

    // -----------------
    // Airdrop/Transactions
    // -----------------
    pub fn airdrop_chain(
        &self,
        pubkey: &Pubkey,
        lamports: u64,
    ) -> anyhow::Result<()> {
        Self::airdrop(&self.chain_client, pubkey, lamports, self.commitment)
    }

    pub fn airdrop_ephem(
        &self,
        pubkey: &Pubkey,
        lamports: u64,
    ) -> anyhow::Result<()> {
        Self::airdrop(&self.ephem_client, pubkey, lamports, self.commitment)
    }

    pub fn airdrop(
        rpc_client: &RpcClient,
        pubkey: &Pubkey,
        lamports: u64,
        commitment_config: CommitmentConfig,
    ) -> anyhow::Result<()> {
        let sig = rpc_client.request_airdrop(pubkey, lamports).with_context(
            || format!("Failed to airdrop chain account '{:?}'", pubkey),
        )?;

        let succeeded =
            Self::confirm_transaction(&sig, rpc_client, commitment_config)
                .with_context(|| {
                    format!(
                        "Failed to confirm airdrop chain account '{:?}'",
                        pubkey
                    )
                })?;
        if !succeeded {
            return Err(anyhow::anyhow!(
                "Failed to airdrop chain account '{:?}'",
                pubkey
            ));
        }
        Ok(())
    }

    pub fn confirm_transaction_chain(
        &self,
        sig: &Signature,
    ) -> Result<bool, client_error::Error> {
        Self::confirm_transaction(sig, &self.chain_client, self.commitment)
    }

    pub fn confirm_transaction_ephem(
        &self,
        sig: &Signature,
    ) -> Result<bool, client_error::Error> {
        Self::confirm_transaction(sig, &self.ephem_client, self.commitment)
    }

    pub fn confirm_transaction(
        sig: &Signature,
        rpc_client: &RpcClient,
        commitment_config: CommitmentConfig,
    ) -> Result<bool, client_error::Error> {
        // Wait for the transaction to be confirmed (up to 1 sec)
        let mut count = 0;
        loop {
            match rpc_client
                .confirm_transaction_with_commitment(sig, commitment_config)
            {
                Ok(res) => {
                    return Ok(res.value);
                }
                Err(err) => {
                    count += 1;
                    if count >= 5 {
                        return Err(err);
                    } else {
                        sleep(Duration::from_millis(200));
                    }
                }
            }
        }
    }

    // -----------------
    // Log Extractors
    // -----------------
    pub fn extract_scheduled_commit_sent_signature(
        &self,
        logs: &[String],
    ) -> Option<Signature> {
        // ScheduledCommitSent signature: <signature>
        for log in logs {
            if log.starts_with("ScheduledCommitSent signature: ") {
                let commit_sig =
                    log.split_whitespace().last().expect("No signature found");
                return Signature::from_str(commit_sig).ok();
            }
        }
        None
    }

    pub fn extract_sent_commit_info(
        &self,
        logs: &[String],
    ) -> (Vec<Pubkey>, Vec<Pubkey>, Vec<Signature>) {
        // ScheduledCommitSent included: [6ZQpzi8X2jku3C2ERgZB8hzhQ55VHLm8yZZLwTpMzHw3, 3Q49KuvoEGzGWBsbh2xgrKog66be3UM1aDEsHq7Ym4pr]
        // ScheduledCommitSent excluded: []
        // ScheduledCommitSent signature[0]: g1E7PyWZ3UHFZMJW5KqQsgoZX9PzALh4eekzjg7oGqeDPxEDfipEmV8LtTbb8EbqZfDGEaA9xbd1fADrGDGZZyi
        let mut included = vec![];
        let mut excluded = vec![];
        let mut signgatures = vec![];

        fn pubkeys_from_log_line(log: &str) -> Vec<Pubkey> {
            log.trim_end_matches(']')
                .split_whitespace()
                .skip(2)
                .flat_map(|p| {
                    let key = p
                        .trim()
                        .trim_matches(',')
                        .trim_matches('[')
                        .trim_matches(']');
                    if key.is_empty() {
                        None
                    } else {
                        Pubkey::from_str(key).ok()
                    }
                })
                .collect::<Vec<Pubkey>>()
        }

        for log in logs {
            if log.starts_with("ScheduledCommitSent included: ") {
                included = pubkeys_from_log_line(log)
            } else if log.starts_with("ScheduledCommitSent excluded: ") {
                excluded = pubkeys_from_log_line(log)
            } else if log.starts_with("ScheduledCommitSent signature[") {
                let commit_sig = log
                    .trim_end_matches(']')
                    .split_whitespace()
                    .last()
                    .and_then(|s| Signature::from_str(s).ok());
                if let Some(commit_sig) = commit_sig {
                    signgatures.push(commit_sig);
                }
            }
        }
        (included, excluded, signgatures)
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
