use std::{str::FromStr, thread::sleep, time::Duration};

use anyhow::{Context, Result};
use borsh::BorshDeserialize;
use log::*;
use solana_rpc_client::{
    nonblocking,
    rpc_client::{GetConfirmedSignaturesForAddress2Config, RpcClient},
};
use solana_rpc_client_api::{
    client_error::{self, Error as ClientError, ErrorKind as ClientErrorKind},
    config::{RpcSendTransactionConfig, RpcTransactionConfig},
};
#[allow(unused_imports)]
use solana_sdk::signer::SeedDerivable;
use solana_sdk::{
    account::Account,
    clock::Slot,
    commitment_config::CommitmentConfig,
    hash::Hash,
    instruction::Instruction,
    pubkey::Pubkey,
    rent::Rent,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::{Transaction, TransactionError},
};
use solana_transaction_status::{
    EncodedConfirmedBlock, EncodedConfirmedTransactionWithStatusMeta,
    UiTransactionEncoding,
};

use crate::{
    dlp_interface,
    transactions::{
        confirm_transaction, send_and_confirm_instructions_with_payer,
        send_and_confirm_transaction, send_instructions_with_payer,
        send_transaction,
    },
};

const URL_CHAIN: &str = "http://localhost:7799";
const WS_URL_CHAIN: &str = "ws://localhost:7800";
const URL_EPHEM: &str = "http://localhost:8899";

fn async_rpc_client(
    rpc_client: &RpcClient,
) -> nonblocking::rpc_client::RpcClient {
    nonblocking::rpc_client::RpcClient::new_with_commitment(
        rpc_client.url(),
        rpc_client.commitment(),
    )
}

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
}

impl IntegrationTestContext {
    pub fn try_new_ephem_only() -> Result<Self> {
        color_backtrace::install();

        let commitment = CommitmentConfig::confirmed();
        let ephem_client = RpcClient::new_with_commitment(
            Self::url_ephem().to_string(),
            commitment,
        );
        let validator_identity = ephem_client.get_identity()?;
        Ok(Self {
            commitment,
            chain_client: None,
            ephem_client: Some(ephem_client),
            ephem_validator_identity: Some(validator_identity),
        })
    }

    pub fn try_new_chain_only() -> Result<Self> {
        color_backtrace::install();

        let commitment = CommitmentConfig::confirmed();
        let chain_client = RpcClient::new_with_commitment(
            Self::url_chain().to_string(),
            commitment,
        );
        Ok(Self {
            commitment,
            chain_client: Some(chain_client),
            ephem_client: None,
            ephem_validator_identity: None,
        })
    }

    pub fn try_new() -> Result<Self> {
        Self::try_new_with_ephem_port(8899)
    }

    pub fn try_new_with_ephem_port(port: u16) -> Result<Self> {
        color_backtrace::install();

        let commitment = CommitmentConfig::confirmed();

        let chain_client = RpcClient::new_with_commitment(
            Self::url_chain().to_string(),
            commitment,
        );
        let ephem_client = RpcClient::new_with_commitment(
            Self::url_local_ephem_at_port(port).to_string(),
            commitment,
        );
        let validator_identity = ephem_client.get_identity()?;

        Ok(Self {
            commitment,
            chain_client: Some(chain_client),
            ephem_client: Some(ephem_client),
            ephem_validator_identity: Some(validator_identity),
        })
    }

    // -----------------
    // Fetch Logs
    // -----------------
    pub fn fetch_ephemeral_logs(&self, sig: Signature) -> Option<Vec<String>> {
        self.fetch_logs(sig, self.ephem_client.as_ref(), "ephemeral")
    }

    pub fn fetch_chain_logs(&self, sig: Signature) -> Option<Vec<String>> {
        self.fetch_logs(sig, self.chain_client.as_ref(), "chain")
    }

    fn fetch_logs(
        &self,
        sig: Signature,
        rpc_client: Option<&RpcClient>,
        label: &str,
    ) -> Option<Vec<String>> {
        let rpc_client = rpc_client.or(self.chain_client.as_ref())?;

        // Try this up to 50 times since devnet here returns the version response instead of
        // the EncodedConfirmedTransactionWithStatusMeta at times
        for idx in 1..=50 {
            let status = match rpc_client.get_transaction_with_config(
                &sig,
                RpcTransactionConfig {
                    commitment: Some(self.commitment),
                    ..Default::default()
                },
            ) {
                Ok(status) => status,
                Err(err) => {
                    if idx % 10 == 0 {
                        warn!(
                            "Failed to fetch transaction from {}: {:?}",
                            label, err
                        );
                    }
                    sleep(Duration::from_millis(400));
                    continue;
                }
            };
            return Option::<Vec<String>>::from(
                status
                    .transaction
                    .meta
                    .as_ref()
                    .with_context(|| {
                        format!(
                            "No transaction meta found for signature {:?}: {:?}",
                            sig, status
                        )
                    })
                    .unwrap()
                    .log_messages
                    .clone(),
            );
        }
        None
    }

    pub fn dump_chain_logs(&self, sig: Signature) {
        let Some(logs) = self.fetch_chain_logs(sig) else {
            eprintln!("No chain logs found for '{}'", sig);
            return;
        };

        eprintln!("Chain Logs for '{}':\n{:#?}", sig, logs);
    }

    pub fn dump_ephemeral_logs(&self, sig: Signature) {
        let Some(logs) = self.fetch_ephemeral_logs(sig) else {
            eprintln!("No ephemeral logs found for '{}'", sig);
            return;
        };
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

    pub fn try_chain_client_async(
        &self,
    ) -> anyhow::Result<nonblocking::rpc_client::RpcClient> {
        let Some(chain_client) = self.chain_client.as_ref() else {
            return Err(anyhow::anyhow!("Chain client not available"));
        };
        Ok(async_rpc_client(chain_client))
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

    pub fn fetch_chain_account_struct<T>(&self, pubkey: Pubkey) -> Result<T>
    where
        T: BorshDeserialize,
    {
        self.try_chain_client().and_then(|chain_client| {
            Self::fetch_account_struct(
                chain_client,
                pubkey,
                self.commitment,
                "chain",
            )
        })
    }

    pub fn fetch_ephem_account_struct<T>(&self, pubkey: Pubkey) -> Result<T>
    where
        T: BorshDeserialize,
    {
        self.try_ephem_client().and_then(|chain_client| {
            Self::fetch_account_struct(
                chain_client,
                pubkey,
                self.commitment,
                "ephem",
            )
        })
    }

    fn fetch_account_struct<T>(
        rpc_client: &RpcClient,
        pubkey: Pubkey,
        commitment: CommitmentConfig,
        cluster: &str,
    ) -> Result<T>
    where
        T: BorshDeserialize,
    {
        let account = rpc_client
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
            })?;

        T::try_from_slice(&account.data).with_context(|| {
            anyhow::anyhow!(
                "Failed to deserialize account: {}, cluster: {}",
                pubkey,
                cluster
            )
        })
    }

    pub fn fetch_chain_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
    ) -> anyhow::Result<Vec<Option<Account>>> {
        self.try_chain_client().and_then(|chain_client| {
            Self::fetch_multiple_accounts(
                chain_client,
                pubkeys,
                self.commitment,
                "chain",
            )
        })
    }

    pub fn fetch_ephem_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
    ) -> anyhow::Result<Vec<Option<Account>>> {
        self.try_ephem_client().and_then(|ephem_client| {
            Self::fetch_multiple_accounts(
                ephem_client,
                pubkeys,
                self.commitment,
                "ephemeral",
            )
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

    fn fetch_multiple_accounts(
        rpc_client: &RpcClient,
        pubkeys: &[Pubkey],
        commitment: CommitmentConfig,
        cluster: &str,
    ) -> anyhow::Result<Vec<Option<Account>>> {
        Ok(rpc_client
            .get_multiple_accounts_with_commitment(pubkeys, commitment)
            .with_context(|| {
                format!(
                    "Failed to fetch {} multiple account data for '{:?}'",
                    cluster, pubkeys
                )
            })?
            .value)
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
    /// Airdrop lamports to the payer on-chain account and
    /// then top up the ephemeral fee balance with half of that
    pub fn airdrop_chain_escrowed(
        &self,
        payer: &Keypair,
        lamports: u64,
    ) -> anyhow::Result<(Signature, Signature, Pubkey, Pubkey, u64)> {
        // 1. Airdrop funds to the payer itself
        let airdrop_sig = self.airdrop_chain(&payer.pubkey(), lamports)?;
        debug!(
            "Airdropped {} lamports to {} ({})",
            lamports,
            payer.pubkey(),
            airdrop_sig
        );

        // 2. Top up the ephemeral fee balance account from the payer
        let topup_lamports = lamports / 2;

        let ixs = dlp_interface::create_topup_ixs(
            payer.pubkey(),
            payer.pubkey(),
            topup_lamports,
            self.ephem_validator_identity,
        );
        let (escrow_sig, confirmed) =
            self.send_and_confirm_instructions_with_payer_chain(&ixs, payer)?;
        assert!(confirmed, "Failed to confirm escrow airdrop");

        let (ephemeral_balance_pda, deleg_record) =
            dlp_interface::escrow_pdas(&payer.pubkey());

        let escrow_lamports =
            topup_lamports + Rent::default().minimum_balance(0);
        Ok((
            airdrop_sig,
            escrow_sig,
            ephemeral_balance_pda,
            deleg_record,
            escrow_lamports,
        ))
    }

    /// Airdrop lamports to the payer on-chain account and
    /// then delegates it as on-curve
    pub fn airdrop_chain_and_delegate(
        &self,
        payer_chain: &Keypair,
        payer_ephem: &Keypair,
        lamports: u64,
    ) -> anyhow::Result<(Signature, Signature)> {
        // 1. Airdrop funds to the payer we will clone into the ephem
        let payer_ephem_airdrop_sig =
            self.airdrop_chain(&payer_ephem.pubkey(), lamports)?;
        debug!(
            "Airdropped {} lamports to ephem payer {} ({})",
            lamports,
            payer_ephem.pubkey(),
            payer_ephem_airdrop_sig
        );

        // 2.Delegate the ephem payer
        let delegated_already = self
            .fetch_chain_account_owner(payer_ephem.pubkey())
            .map(|owner| owner.eq(&dlp::id()))
            .unwrap_or(false);
        let deleg_sig = if !delegated_already {
            let (deleg_sig, confirmed) =
                self.delegate_account(payer_chain, payer_ephem)?;

            assert!(confirmed, "Failed to confirm airdrop delegation");
            debug!("Delegated payer {}", payer_ephem.pubkey());
            deleg_sig
        } else {
            debug!(
                "Ephem payer {} already delegated, skipping",
                payer_ephem.pubkey()
            );
            Signature::default()
        };

        Ok((payer_ephem_airdrop_sig, deleg_sig))
    }

    pub fn delegate_account(
        &self,
        payer_chain: &Keypair,
        payer_ephem: &Keypair,
    ) -> anyhow::Result<(Signature, bool)> {
        let ixs = dlp_interface::create_delegate_ixs(
            // We change the owner of the ephem account, thus cannot use it as payer
            payer_chain.pubkey(),
            payer_ephem.pubkey(),
            self.ephem_validator_identity,
        );
        let mut tx =
            Transaction::new_with_payer(&ixs, Some(&payer_chain.pubkey()));
        let (deleg_sig, confirmed) = self.send_and_confirm_transaction_chain(
            &mut tx,
            &[payer_chain, payer_ephem],
        )?;
        Ok((deleg_sig, confirmed))
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
            confirm_transaction(&sig, rpc_client, commitment_config, None)
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
        tx: Option<&Transaction>,
    ) -> Result<bool, client_error::Error> {
        confirm_transaction(
            sig,
            self.try_chain_client().map_err(|err| client_error::Error {
                request: None,
                kind: client_error::ErrorKind::Custom(err.to_string()),
            })?,
            self.commitment,
            tx,
        )
    }

    pub fn confirm_transaction_ephem(
        &self,
        sig: &Signature,
        tx: Option<&Transaction>,
    ) -> Result<bool, client_error::Error> {
        confirm_transaction(
            sig,
            self.try_ephem_client().map_err(|err| client_error::Error {
                request: None,
                kind: client_error::ErrorKind::Custom(err.to_string()),
            })?,
            self.commitment,
            tx,
        )
    }

    pub fn send_transaction_ephem(
        &self,
        tx: &mut Transaction,
        signers: &[&Keypair],
    ) -> Result<Signature, client_error::Error> {
        send_transaction(
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
        send_transaction(
            self.try_chain_client().map_err(|err| client_error::Error {
                request: None,
                kind: client_error::ErrorKind::Custom(err.to_string()),
            })?,
            tx,
            signers,
        )
    }

    pub fn send_instructions_with_payer_chain(
        &self,
        ixs: &[Instruction],
        payer: &Keypair,
    ) -> Result<(Signature, Transaction), client_error::Error> {
        send_instructions_with_payer(
            self.try_chain_client().map_err(|err| client_error::Error {
                request: None,
                kind: client_error::ErrorKind::Custom(err.to_string()),
            })?,
            ixs,
            payer,
        )
    }

    pub fn send_and_confirm_transaction_ephem(
        &self,
        tx: &mut Transaction,
        signers: &[&Keypair],
    ) -> Result<(Signature, bool), anyhow::Error> {
        self.try_ephem_client().and_then(|ephem_client| {
            send_and_confirm_transaction(
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
            send_and_confirm_transaction(
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

    pub fn send_and_confirm_instructions_with_payer_ephem(
        &self,
        ixs: &[Instruction],
        payer: &Keypair,
    ) -> Result<(Signature, bool), anyhow::Error> {
        self.try_ephem_client().and_then(|ephem_client| {
            send_and_confirm_instructions_with_payer(
                ephem_client,
                ixs,
                payer,
                self.commitment,
                "ephemeral",
            )
            .with_context(|| {
                format!(
                    "Failed to confirm ephem instructions with payer '{:?}'",
                    payer.pubkey()
                )
            })
        })
    }

    pub fn send_and_confirm_instructions_with_payer_chain(
        &self,
        ixs: &[Instruction],
        payer: &Keypair,
    ) -> Result<(Signature, bool), anyhow::Error> {
        self.try_chain_client().and_then(|chain_client| {
            send_and_confirm_instructions_with_payer(
                chain_client,
                ixs,
                payer,
                self.commitment,
                "chain",
            )
            .with_context(|| {
                format!(
                    "Failed to confirm chain instructions with payer '{:?}'",
                    payer.pubkey()
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
        let sig = rpc_client.send_transaction_with_config(
            tx,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )?;
        rpc_client.confirm_transaction_with_commitment(
            &sig,
            CommitmentConfig::confirmed(),
        )?;
        Ok(sig)
    }

    pub fn send_instructions_with_payer(
        rpc_client: &RpcClient,
        ixs: &[Instruction],
        payer: &Keypair,
    ) -> Result<Signature, client_error::Error> {
        let blockhash = rpc_client.get_latest_blockhash()?;
        let mut tx = Transaction::new_with_payer(ixs, Some(&payer.pubkey()));
        tx.sign(&[payer], blockhash);
        Self::send_transaction(rpc_client, &mut tx, &[payer])
    }

    pub fn send_and_confirm_transaction(
        rpc_client: &RpcClient,
        tx: &mut Transaction,
        signers: &[&Keypair],
        commitment: CommitmentConfig,
    ) -> Result<(Signature, bool), client_error::Error> {
        let sig = Self::send_transaction(rpc_client, tx, signers)?;
        confirm_transaction(&sig, rpc_client, commitment, Some(tx))
            .map(|confirmed| (sig, confirmed))
    }

    pub fn send_and_confirm_instructions_with_payer(
        &self,
        rpc_client: &RpcClient,
        ixs: &[Instruction],
        payer: &Keypair,
        commitment: CommitmentConfig,
    ) -> Result<(Signature, bool), client_error::Error> {
        let sig = Self::send_instructions_with_payer(rpc_client, ixs, payer)?;
        debug!("Confirming transaction with signature: {}", sig);
        confirm_transaction(&sig, rpc_client, commitment, None)
            .map(|confirmed| (sig, confirmed))
            .inspect_err(|_| {
                self.dump_ephemeral_logs(sig);
                self.dump_chain_logs(sig);
            })
    }

    pub fn get_transaction_chain(
        &self,
        sig: &Signature,
    ) -> Result<EncodedConfirmedTransactionWithStatusMeta, anyhow::Error> {
        self.try_chain_client().and_then(|client| {
            client
                .get_transaction(sig, UiTransactionEncoding::Base58)
                .map_err(|e| anyhow::anyhow!("{}", e))
        })
    }

    pub fn get_transaction_ephem(
        &self,
        sig: &Signature,
    ) -> Result<EncodedConfirmedTransactionWithStatusMeta, anyhow::Error> {
        self.try_ephem_client().and_then(|client| {
            client
                .get_transaction(sig, UiTransactionEncoding::Base58)
                .map_err(|e| anyhow::anyhow!("{}", e))
        })
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
    pub fn get_slot_ephem(&self) -> Result<Slot> {
        self.try_ephem_client().and_then(|ephem_client| {
            ephem_client
                .get_slot()
                .map_err(|e| anyhow::anyhow!("{}", e))
        })
    }

    pub fn get_slot_chain(&self) -> Result<Slot> {
        self.try_chain_client().and_then(|chain_client| {
            chain_client
                .get_slot()
                .map_err(|e| anyhow::anyhow!("{}", e))
        })
    }
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

    pub fn try_get_latest_blockhash_ephem(&self) -> Result<Hash> {
        self.try_ephem_client().and_then(Self::get_latest_blockhash)
    }

    pub fn try_get_latest_blockhash_chain(&self) -> Result<Hash> {
        self.try_chain_client().and_then(Self::get_latest_blockhash)
    }

    fn get_latest_blockhash(rpc_client: &RpcClient) -> Result<Hash> {
        rpc_client
            .get_latest_blockhash()
            .map_err(|e| anyhow::anyhow!("Failed to get blockhash{}", e))
    }

    // -----------------
    // Block
    // -----------------
    pub fn try_get_block_ephem(
        &self,
        slot: Slot,
    ) -> Result<EncodedConfirmedBlock> {
        self.try_ephem_client()
            .and_then(|ephem_client| Self::get_block(ephem_client, slot))
    }
    pub fn try_get_block_chain(
        &self,
        slot: Slot,
    ) -> Result<EncodedConfirmedBlock> {
        self.try_chain_client()
            .and_then(|chain_client| Self::get_block(chain_client, slot))
    }
    fn get_block(
        rpc_client: &RpcClient,
        slot: Slot,
    ) -> Result<EncodedConfirmedBlock> {
        rpc_client
            .get_block(slot)
            .map_err(|e| anyhow::anyhow!("Failed to get block: {}", e))
    }

    // -----------------
    // Blocktime
    // -----------------
    pub fn try_get_block_time_ephem(&self, slot: Slot) -> Result<i64> {
        self.try_ephem_client()
            .and_then(|ephem_client| Self::get_block_time(ephem_client, slot))
    }
    pub fn try_get_block_time_chain(&self, slot: Slot) -> Result<i64> {
        self.try_chain_client()
            .and_then(|chain_client| Self::get_block_time(chain_client, slot))
    }
    fn get_block_time(rpc_client: &RpcClient, slot: Slot) -> Result<i64> {
        rpc_client
            .get_block_time(slot)
            .map_err(|e| anyhow::anyhow!("Failed to get blocktime: {}", e))
    }

    // -----------------
    // RPC Clients
    // -----------------
    pub fn url_ephem() -> &'static str {
        URL_EPHEM
    }
    pub fn url_local_ephem_at_port(port: u16) -> String {
        format!("http://localhost:{}", port)
    }
    pub fn url_chain() -> &'static str {
        URL_CHAIN
    }
    pub fn ws_url_chain() -> &'static str {
        WS_URL_CHAIN
    }
}
