use std::{str::FromStr, thread::sleep, time::Duration};

use anyhow::{Context, Result};
use schedulecommit_program::api::{
    delegate_account_cpi_instruction, init_account_instruction, pda_and_bump,
};
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::{
    RpcSendTransactionConfig, RpcTransactionConfig,
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
                // let payer = Keypair::new();
                chain_client
                    .request_airdrop(&payer.pubkey(), LAMPORTS_PER_SOL * 100)
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
            .with_context(|| "Failed to delegate committees")
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
                .unwrap_or(&self.chain_client)
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
