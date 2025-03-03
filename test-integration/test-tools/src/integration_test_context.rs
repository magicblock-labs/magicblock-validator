use std::{str::FromStr, thread::sleep, time::Duration};

use anyhow::{Context, Result};
use solana_rpc_client::rpc_client::{
    GetConfirmedSignaturesForAddress2Config, RpcClient,
};
use solana_rpc_client_api::{
    client_error,
    client_error::{Error as ClientError, ErrorKind as ClientErrorKind},
    config::{RpcSendTransactionConfig, RpcTransactionConfig},
};
#[allow(unused_imports)]
use solana_sdk::signer::SeedDerivable;
use solana_sdk::{
    account::Account,
    clock::Slot,
    commitment_config::CommitmentConfig,
    hash::Hash,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    transaction::{Transaction, TransactionError},
};

const URL_CHAIN: &str = "http://localhost:7799";
const URL_EPHEM: &str = "http://localhost:8899";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionStatusWithSignature {
    pub signature: String,
    pub slot: Slot,
    pub err: Option<TransactionError>,
}

impl TransactionStatusWithSignature {
    pub fn signature(&self) -> Signature {
        Signature::from_str(&self.signature).unwrap()
    }

    pub fn has_error(&self) -> bool {
        self.err.is_some()
    }
}

pub struct IntegrationTestContext {
    pub commitment: CommitmentConfig,
    pub chain_client: Option<RpcClient>,
    pub ephem_client: Option<RpcClient>,
    pub ephem_validator_identity: Option<Pubkey>,
    pub chain_blockhash: Option<Hash>,
    pub ephem_blockhash: Option<Hash>,
}

impl IntegrationTestContext {
    pub fn try_new_ephem_only() -> Result<Self> {
        let commitment = CommitmentConfig::confirmed();
        let ephem_client = RpcClient::new_with_commitment(
            Self::url_ephem().to_string(),
            commitment,
        );
        let validator_identity = ephem_client.get_identity()?;
        let ephem_blockhash = ephem_client.get_latest_blockhash()?;
        Ok(Self {
            commitment,
            chain_client: None,
            ephem_client: Some(ephem_client),
            ephem_validator_identity: Some(validator_identity),
            chain_blockhash: None,
            ephem_blockhash: Some(ephem_blockhash),
        })
    }

    pub fn try_new_chain_only() -> Result<Self> {
        let commitment = CommitmentConfig::confirmed();
        let chain_client = RpcClient::new_with_commitment(
            Self::url_chain().to_string(),
            commitment,
        );
        let chain_blockhash = chain_client.get_latest_blockhash()?;
        Ok(Self {
            commitment,
            chain_client: Some(chain_client),
            ephem_client: None,
            ephem_validator_identity: None,
            chain_blockhash: Some(chain_blockhash),
            ephem_blockhash: None,
        })
    }

    pub fn try_new() -> Result<Self> {
        let commitment = CommitmentConfig::confirmed();

        let chain_client = RpcClient::new_with_commitment(
            Self::url_chain().to_string(),
            commitment,
        );
        let ephem_client = RpcClient::new_with_commitment(
            Self::url_ephem().to_string(),
            commitment,
        );
        let validator_identity = chain_client.get_identity()?;
        let chain_blockhash = chain_client.get_latest_blockhash()?;
        let ephem_blockhash = ephem_client.get_latest_blockhash()?;

        Ok(Self {
            commitment,
            chain_client: Some(chain_client),
            ephem_client: Some(ephem_client),
            ephem_validator_identity: Some(validator_identity),
            chain_blockhash: Some(chain_blockhash),
            ephem_blockhash: Some(ephem_blockhash),
        })
    }

    // -----------------
    // Fetch Logs
    // -----------------
    pub fn fetch_ephemeral_logs(&self, sig: Signature) -> Option<Vec<String>> {
        self.fetch_logs(sig, self.ephem_client.as_ref())
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
    pub fn try_chain_client(&self) -> anyhow::Result<&RpcClient> {
        let Some(chain_client) = self.chain_client.as_ref() else {
            return Err(anyhow::anyhow!("Chain client not available"));
        };
        Ok(chain_client)
    }

    pub fn try_ephem_client(&self) -> anyhow::Result<&RpcClient> {
        let Some(ephem_client) = self.ephem_client.as_ref() else {
            return Err(anyhow::anyhow!("Ephem client not available"));
        };
        Ok(ephem_client)
    }

    pub fn fetch_ephem_account_data(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<Vec<u8>> {
        self.fetch_ephem_account(pubkey).map(|account| account.data)
    }

    pub fn fetch_chain_account_data(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<Vec<u8>> {
        self.fetch_chain_account(pubkey).map(|account| account.data)
    }

    pub fn fetch_ephem_account(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<Account> {
        self.try_ephem_client().and_then(|ephem_client| {
            Self::fetch_account(
                ephem_client,
                pubkey,
                self.commitment,
                "ephemeral",
            )
        })
    }

    pub fn fetch_chain_account(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<Account> {
        self.try_chain_client().and_then(|chain_client| {
            Self::fetch_account(chain_client, pubkey, self.commitment, "chain")
        })
    }

    fn fetch_account(
        rpc_client: &RpcClient,
        pubkey: Pubkey,
        commitment: CommitmentConfig,
        cluster: &str,
    ) -> anyhow::Result<Account> {
        rpc_client
            .get_account_with_commitment(&pubkey, commitment)
            .with_context(|| {
                format!(
                    "Failed to fetch {} account data for '{:?}'",
                    cluster, pubkey
                )
            })?
            .value
            .ok_or_else(|| {
                anyhow::anyhow!("Account '{}' not found on {}", pubkey, cluster)
            })
    }

    pub fn fetch_ephem_account_balance(
        &self,
        pubkey: &Pubkey,
    ) -> anyhow::Result<u64> {
        self.try_ephem_client().and_then(|ephem_client| {
            ephem_client
                .get_balance_with_commitment(pubkey, self.commitment)
                .map(|balance| balance.value)
                .with_context(|| {
                    format!(
                        "Failed to fetch ephemeral account balance for '{:?}'",
                        pubkey
                    )
                })
        })
    }

    pub fn fetch_chain_account_balance(
        &self,
        pubkey: &Pubkey,
    ) -> anyhow::Result<u64> {
        self.try_chain_client()?
            .get_balance_with_commitment(pubkey, self.commitment)
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
        self.fetch_ephem_account(pubkey)
            .map(|account| account.owner)
    }

    pub fn fetch_chain_account_owner(
        &self,
        pubkey: Pubkey,
    ) -> anyhow::Result<Pubkey> {
        self.fetch_chain_account(pubkey)
            .map(|account| account.owner)
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
        self.try_ephem_client().and_then(|ephem_client| {
            Self::airdrop(ephem_client, pubkey, lamports, self.commitment)
        })
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
        Self::confirm_transaction(
            sig,
            self.try_ephem_client().map_err(|err| client_error::Error {
                request: None,
                kind: client_error::ErrorKind::Custom(err.to_string()),
            })?,
            self.commitment,
        )
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

    pub fn send_transaction_ephem(
        &self,
        tx: &mut Transaction,
        signers: &[&Keypair],
    ) -> Result<Signature, client_error::Error> {
        Self::send_transaction(
            self.try_ephem_client().map_err(|err| client_error::Error {
                request: None,
                kind: client_error::ErrorKind::Custom(err.to_string()),
            })?,
            tx,
            signers,
        )
    }

    pub fn send_transaction_chain(
        &self,
        tx: &mut Transaction,
        signers: &[&Keypair],
    ) -> Result<Signature, client_error::Error> {
        Self::send_transaction(
            self.try_chain_client().map_err(|err| client_error::Error {
                request: None,
                kind: client_error::ErrorKind::Custom(err.to_string()),
            })?,
            tx,
            signers,
        )
    }

    pub fn send_and_confirm_transaction_ephem(
        &self,
        tx: &mut Transaction,
        signers: &[&Keypair],
    ) -> Result<(Signature, bool), anyhow::Error> {
        self.try_ephem_client().and_then(|ephem_client| {
            Self::send_and_confirm_transaction(
                ephem_client,
                tx,
                signers,
                self.commitment,
            )
            .with_context(|| {
                format!(
                    "Failed to confirm ephem transaction '{:?}'",
                    tx.signatures[0]
                )
            })
        })
    }

    pub fn send_and_confirm_transaction_chain(
        &self,
        tx: &mut Transaction,
        signers: &[&Keypair],
    ) -> Result<(Signature, bool), anyhow::Error> {
        self.try_chain_client().and_then(|chain_client| {
            Self::send_and_confirm_transaction(
                chain_client,
                tx,
                signers,
                self.commitment,
            )
            .with_context(|| {
                format!(
                    "Failed to confirm chain transaction '{:?}'",
                    tx.signatures[0]
                )
            })
        })
    }

    pub fn send_transaction(
        rpc_client: &RpcClient,
        tx: &mut Transaction,
        signers: &[&Keypair],
    ) -> Result<Signature, client_error::Error> {
        let blockhash = rpc_client.get_latest_blockhash()?;
        tx.sign(signers, blockhash);
        let sig = rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                tx,
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )?;
        Ok(sig)
    }

    pub fn send_and_confirm_transaction(
        rpc_client: &RpcClient,
        tx: &mut Transaction,
        signers: &[&Keypair],
        commitment: CommitmentConfig,
    ) -> Result<(Signature, bool), client_error::Error> {
        let sig = Self::send_transaction(rpc_client, tx, signers)?;
        Self::confirm_transaction(&sig, rpc_client, commitment)
            .map(|confirmed| (sig, confirmed))
    }

    // -----------------
    // Transaction Queries
    // -----------------
    pub fn get_signaturestats_for_address_ephem(
        &self,
        address: &Pubkey,
    ) -> Result<Vec<TransactionStatusWithSignature>> {
        self.try_ephem_client().and_then(|ephem_client| {
            Self::get_signaturestats_for_address(
                ephem_client,
                address,
                self.commitment,
            )
        })
    }

    pub fn get_signaturestats_for_address_chain(
        &self,
        address: &Pubkey,
    ) -> Result<Vec<TransactionStatusWithSignature>> {
        self.try_chain_client().and_then(|chain_client| {
            Self::get_signaturestats_for_address(
                chain_client,
                address,
                self.commitment,
            )
        })
    }

    fn get_signaturestats_for_address(
        rpc_client: &RpcClient,
        address: &Pubkey,
        commitment: CommitmentConfig,
    ) -> Result<Vec<TransactionStatusWithSignature>> {
        let res = rpc_client
            .get_signatures_for_address_with_config(
                address,
                GetConfirmedSignaturesForAddress2Config {
                    commitment: Some(commitment),
                    ..Default::default()
                },
            )
            .map(|status| {
                status
                    .into_iter()
                    .map(|x| TransactionStatusWithSignature {
                        signature: x.signature,
                        slot: x.slot,
                        err: x.err,
                    })
                    .collect()
            })?;
        Ok(res)
    }

    // -----------------
    // Slot
    // -----------------
    pub fn wait_for_next_slot_ephem(&self) -> Result<Slot> {
        self.try_ephem_client().and_then(Self::wait_for_next_slot)
    }

    pub fn wait_for_delta_slot_ephem(&self, delta: Slot) -> Result<Slot> {
        self.try_ephem_client().and_then(|ephem_client| {
            Self::wait_for_delta_slot(ephem_client, delta)
        })
    }

    pub fn wait_for_slot_ephem(&self, target_slot: Slot) -> Result<Slot> {
        self.try_ephem_client().and_then(|ephem_client| {
            Self::wait_until_slot(ephem_client, target_slot)
        })
    }

    pub fn wait_for_next_slot_chain(&self) -> Result<Slot> {
        self.try_chain_client().and_then(Self::wait_for_next_slot)
    }

    fn wait_for_next_slot(rpc_client: &RpcClient) -> Result<Slot> {
        let initial_slot = rpc_client.get_slot()?;
        Self::wait_until_slot(rpc_client, initial_slot + 1)
    }

    fn wait_for_delta_slot(
        rpc_client: &RpcClient,
        delta: Slot,
    ) -> Result<Slot> {
        let initial_slot = rpc_client.get_slot()?;
        Self::wait_until_slot(rpc_client, initial_slot + delta)
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

    // -----------------
    // Blockhash
    // -----------------
    pub fn get_all_blockhashes_ephem(&self) -> Result<Vec<String>> {
        self.try_ephem_client().and_then(Self::get_all_blockhashes)
    }

    pub fn get_all_blockhashes_chain(&self) -> Result<Vec<String>> {
        Self::get_all_blockhashes(self.try_chain_client().unwrap())
    }

    fn get_all_blockhashes(rpc_client: &RpcClient) -> Result<Vec<String>> {
        let current_slot = rpc_client.get_slot()?;
        let mut blockhashes = vec![];
        for slot in 0..current_slot {
            let blockhash = rpc_client.get_block(slot)?.blockhash;
            blockhashes.push(blockhash);
        }
        Ok(blockhashes)
    }

    // -----------------
    // RPC Clients
    // -----------------
    pub fn url_ephem() -> &'static str {
        URL_EPHEM
    }
    pub fn url_chain() -> &'static str {
        URL_CHAIN
    }
}
