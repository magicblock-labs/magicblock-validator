use std::{thread::sleep, time::Duration};

use anyhow::{Context, Result};
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::{
    client_error, client_error::Error as ClientError,
    client_error::ErrorKind as ClientErrorKind, config::RpcTransactionConfig,
};

use solana_sdk::clock::Slot;
#[allow(unused_imports)]
use solana_sdk::signer::SeedDerivable;
use solana_sdk::{
    commitment_config::CommitmentConfig, hash::Hash, pubkey::Pubkey,
    signature::Signature,
};

pub struct IntegrationTestContext {
    pub commitment: CommitmentConfig,
    pub chain_client: Option<RpcClient>,
    pub ephem_client: RpcClient,
    pub validator_identity: Pubkey,
    pub chain_blockhash: Option<Hash>,
    pub ephem_blockhash: Hash,
}

// Copy the impl of the ScheduleCommitTestContext here from test-integration/schedulecommit/client/src/schedule_commit_context.rs
// Omit the ones that need committees or whichever else needs fields we don't have here
impl IntegrationTestContext {
    pub fn new_ephem_only() -> Self {
        let commitment = CommitmentConfig::confirmed();
        let ephem_client = RpcClient::new_with_commitment(
            "http://localhost:8899".to_string(),
            commitment,
        );
        let validator_identity = ephem_client.get_identity().unwrap();
        let ephem_blockhash = ephem_client.get_latest_blockhash().unwrap();
        Self {
            commitment,
            chain_client: None,
            ephem_client,
            validator_identity,
            chain_blockhash: None,
            ephem_blockhash,
        }
    }

    pub fn new() -> Self {
        let commitment = CommitmentConfig::confirmed();

        let chain_client = RpcClient::new_with_commitment(
            "http://localhost:7799".to_string(),
            commitment,
        );
        let ephem_client = RpcClient::new_with_commitment(
            "http://localhost:8899".to_string(),
            commitment,
        );
        let validator_identity = chain_client.get_identity().unwrap();
        let chain_blockhash = chain_client.get_latest_blockhash().unwrap();
        let ephem_blockhash = ephem_client.get_latest_blockhash().unwrap();

        Self {
            commitment,
            chain_client: Some(chain_client),
            ephem_client,
            validator_identity,
            chain_blockhash: Some(chain_blockhash),
            ephem_blockhash,
        }
    }

    // -----------------
    // Fetch Logs
    // -----------------
    pub fn fetch_ephemeral_logs(&self, sig: Signature) -> Option<Vec<String>> {
        self.fetch_logs(sig, Some(&self.ephem_client))
    }

    pub fn fetch_chain_logs(&self, sig: Signature) -> Option<Vec<String>> {
        self.fetch_logs(sig, self.chain_client.as_ref())
    }

    fn fetch_logs(
        &self,
        sig: Signature,
        rpc_client: Option<&RpcClient>,
    ) -> Option<Vec<String>> {
        let rpc_client = rpc_client.or(self.chain_client.as_ref())?;

        // Try this up to 10 times since devnet here returns the version response instead of
        // the EncodedConfirmedTransactionWithStatusMeta at times
        for _ in 0..10 {
            let status = match rpc_client.get_transaction_with_config(
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

    pub fn dump_chain_logs(&self, sig: Signature) {
        let logs = self.fetch_chain_logs(sig).unwrap();
        eprintln!("Chain Logs for '{}':\n{:#?}", sig, logs);
    }

    pub fn dump_ephemeral_logs(&self, sig: Signature) {
        let logs = self.fetch_ephemeral_logs(sig).unwrap();
        eprintln!("Ephemeral Logs for '{}':\n{:#?}", sig, logs);
    }

    pub fn assert_chain_logs_contain(&self, sig: Signature, expected: &str) {
        let logs = self.fetch_chain_logs(sig).unwrap();
        assert!(
            self.logs_contain(&logs, expected),
            "Logs do not contain '{}': {:?}",
            expected,
            logs
        );
    }

    pub fn assert_ephemeral_logs_contain(
        &self,
        sig: Signature,
        expected: &str,
    ) {
        let logs = self.fetch_ephemeral_logs(sig).unwrap();
        assert!(
            self.logs_contain(&logs, expected),
            "Logs do not contain '{}': {:?}",
            expected,
            logs
        );
    }

    fn logs_contain(&self, logs: &[String], expected: &str) -> bool {
        logs.iter().any(|log| log.contains(expected))
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

    pub fn try_chain_client(&self) -> anyhow::Result<&RpcClient> {
        let Some(chain_client) = self.chain_client.as_ref() else {
            return Err(anyhow::anyhow!("Chain client not available"));
        };
        Ok(chain_client)
    }

    pub fn fetch_chain_account_data(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<Vec<u8>> {
        self.try_chain_client()?
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
        self.try_chain_client()?
            .get_balance_with_commitment(&pubkey, self.commitment)
            .map(|balance| balance.value)
            .with_context(|| {
                format!(
                    "Failed to fetch chain account balance for '{:?}'",
                    pubkey
                )
            })
    }

    pub fn fetch_ephem_account_owner(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<Pubkey> {
        self.ephem_client
            .get_account(&pubkey)
            .map(|account| account.owner)
            .with_context(|| {
                format!(
                    "Failed to fetch ephemeral account owner for '{:?}'",
                    pubkey
                )
            })
    }

    pub fn fetch_chain_account_owner(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<Pubkey> {
        self.try_chain_client()?
            .get_account(&pubkey)
            .map(|account| account.owner)
            .with_context(|| {
                format!(
                    "Failed to fetch chain account owner for '{:?}'",
                    pubkey
                )
            })
    }

    // -----------------
    // Airdrop
    // -----------------
    pub fn airdrop_chain(
        &self,
        pubkey: &Pubkey,
        lamports: u64,
    ) -> anyhow::Result<Signature> {
        Self::airdrop(
            self.try_chain_client()?,
            pubkey,
            lamports,
            self.commitment,
        )
    }

    pub fn airdrop_ephem(
        &self,
        pubkey: &Pubkey,
        lamports: u64,
    ) -> anyhow::Result<Signature> {
        Self::airdrop(&self.ephem_client, pubkey, lamports, self.commitment)
    }

    pub fn airdrop(
        rpc_client: &RpcClient,
        pubkey: &Pubkey,
        lamports: u64,
        commitment_config: CommitmentConfig,
    ) -> anyhow::Result<Signature> {
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
        Ok(sig)
    }

    // -----------------
    // Transactions
    // -----------------
    pub fn assert_ephemeral_transaction_error(
        &self,
        sig: Signature,
        res: &Result<Signature, ClientError>,
        expected_msg: &str,
    ) {
        Self::assert_transaction_error(res);
        self.assert_ephemeral_logs_contain(sig, expected_msg);
    }

    pub fn assert_chain_transaction_error(
        &self,
        sig: Signature,
        res: &Result<Signature, ClientError>,
        expected_msg: &str,
    ) {
        Self::assert_transaction_error(res);
        self.assert_chain_logs_contain(sig, expected_msg);
    }

    fn assert_transaction_error(res: &Result<Signature, ClientError>) {
        assert!(matches!(
            res,
            Err(ClientError {
                kind: ClientErrorKind::TransactionError(_),
                ..
            })
        ));
    }

    pub fn confirm_transaction_chain(
        &self,
        sig: &Signature,
    ) -> Result<bool, client_error::Error> {
        Self::confirm_transaction(
            sig,
            self.try_chain_client().map_err(|err| client_error::Error {
                request: None,
                kind: client_error::ErrorKind::Custom(err.to_string()),
            })?,
            self.commitment,
        )
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
        // Allow RPC failures to persist for up to 1 sec
        const MAX_FAILURES: u64 = 5;
        const MILLIS_UNTIL_RETRY: u64 = 200;
        let mut failure_count = 0;

        // Allow transactions to take up to 20 seconds to confirm
        const MAX_UNCONFIRMED_COUNT: u64 = 40;
        const MILLIS_UNTIL_RECONFIRM: u64 = 500;
        let mut unconfirmed_count = 0;

        loop {
            match rpc_client
                .confirm_transaction_with_commitment(sig, commitment_config)
            {
                Ok(res) if res.value => {
                    return Ok(res.value);
                }
                Ok(_) => {
                    unconfirmed_count += 1;
                    if unconfirmed_count >= MAX_UNCONFIRMED_COUNT {
                        return Ok(false);
                    } else {
                        sleep(Duration::from_millis(MILLIS_UNTIL_RECONFIRM));
                    }
                }
                Err(err) => {
                    failure_count += 1;
                    if failure_count >= MAX_FAILURES {
                        return Err(err);
                    } else {
                        sleep(Duration::from_millis(MILLIS_UNTIL_RETRY));
                    }
                }
            }
        }
    }

    // -----------------
    // Slot
    // -----------------
    pub fn wait_for_next_slot_ephem(&self) -> Result<Slot> {
        Self::wait_for_next_slot(&self.ephem_client)
    }

    pub fn wait_for_slot_ephem(&self, target_slot: Slot) -> Result<Slot> {
        Self::wait_until_slot(&self.ephem_client, target_slot)
    }

    fn wait_for_next_slot(rpc_client: &RpcClient) -> Result<Slot> {
        let initial_slot = rpc_client.get_slot()?;
        Self::wait_until_slot(rpc_client, initial_slot + 1)
    }

    fn wait_until_slot(
        rpc_client: &RpcClient,
        target_slot: Slot,
    ) -> Result<Slot> {
        let slot = loop {
            let slot = rpc_client.get_slot()?;
            if slot >= target_slot {
                break slot;
            }
            sleep(Duration::from_millis(50));
        };
        Ok(slot)
    }
}

impl Default for IntegrationTestContext {
    fn default() -> Self {
        Self::new()
    }
}
