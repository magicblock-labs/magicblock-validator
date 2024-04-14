#![allow(dead_code)]
use std::{
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use crossbeam_channel::{unbounded, Receiver, Sender};
use jsonrpc_core::{Error, ErrorCode, Metadata, Result, Value};
use log::*;
use sleipnir_bank::bank::Bank;
use sleipnir_rpc_client_api::{
    config::{
        RpcAccountInfoConfig, RpcContextConfig, RpcEncodingConfigWrapper,
        RpcSignatureStatusConfig, RpcSupplyConfig, RpcTransactionConfig,
        UiAccount, UiAccountEncoding,
    },
    custom_error::RpcCustomError,
    filter::RpcFilterType,
    response::{
        OptionalContext, Response as RpcResponse, RpcBlockhash,
        RpcKeyedAccount, RpcSupply,
    },
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_accounts_db::accounts_index::AccountSecondaryIndexes;
use solana_sdk::{
    clock::{Slot, UnixTimestamp},
    epoch_schedule::EpochSchedule,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
};
use solana_transaction_status::{
    EncodedConfirmedTransactionWithStatusMeta, TransactionStatus,
    UiTransactionEncoding,
};

use crate::{
    account_resolver::{encode_account, get_encoded_account},
    filters::{get_filtered_program_accounts, optimize_filters},
    rpc_health::RpcHealth,
    transaction::airdrop_transaction,
    utils::new_response,
    RpcCustomResult,
};

// TODO: send_transaction_service
pub struct TransactionInfo;

// NOTE: from rpc/src/rpc.rs :140
#[derive(Debug, Default, Clone)]
pub struct JsonRpcConfig {
    pub enable_rpc_transaction_history: bool,
    pub enable_extended_tx_metadata_storage: bool,
    pub health_check_slot_distance: u64,
    pub max_multiple_accounts: Option<usize>,
    pub rpc_threads: usize,
    pub rpc_niceness_adj: i8,
    pub full_api: bool,
    pub max_request_body_size: Option<usize>,
    pub account_indexes: AccountSecondaryIndexes,
    /// Disable the health check, used for tests and TestValidator
    pub disable_health_check: bool,

    pub slot_duration: Duration,

    /// Allows updating  Geyser or similar when transactions are processed
    /// Could go into send_transaction_service once we built that
    pub transaction_status_sender: Option<TransactionStatusSender>,
}

// NOTE: from rpc/src/rpc.rs :193
#[derive(Clone)]
pub struct JsonRpcRequestProcessor {
    bank: Arc<Bank>,
    pub(crate) config: JsonRpcConfig,
    transaction_sender: Arc<Mutex<Sender<TransactionInfo>>>,
    pub(crate) health: Arc<RpcHealth>,
    pub faucet_keypair: Arc<Keypair>,
}
impl Metadata for JsonRpcRequestProcessor {}

impl JsonRpcRequestProcessor {
    pub fn new(
        bank: Arc<Bank>,
        health: Arc<RpcHealth>,
        faucet_keypair: Keypair,
        config: JsonRpcConfig,
    ) -> (Self, Receiver<TransactionInfo>) {
        let (sender, receiver) = unbounded();
        let transaction_sender = Arc::new(Mutex::new(sender));
        (
            Self {
                bank,
                config,
                transaction_sender,
                health,
                faucet_keypair: Arc::new(faucet_keypair),
            },
            receiver,
        )
    }

    // -----------------
    // Accounts
    // -----------------
    pub fn get_account_info(
        &self,
        pubkey: &Pubkey,
        config: Option<RpcAccountInfoConfig>,
    ) -> Result<RpcResponse<Option<UiAccount>>> {
        let RpcAccountInfoConfig {
            encoding,
            data_slice,
            ..
        } = config.unwrap_or_default();
        let encoding = encoding.unwrap_or(UiAccountEncoding::Binary);
        let response = get_encoded_account(
            &self.bank, pubkey, encoding, data_slice, None,
        )?;
        Ok(new_response(&self.bank, response))
    }

    pub fn get_multiple_accounts(
        &self,
        pubkeys: Vec<Pubkey>,
        config: Option<RpcAccountInfoConfig>,
    ) -> Result<RpcResponse<Vec<Option<UiAccount>>>> {
        let RpcAccountInfoConfig {
            encoding,
            data_slice,
            ..
        } = config.unwrap_or_default();

        let encoding = encoding.unwrap_or(UiAccountEncoding::Base64);

        let accounts = pubkeys
            .into_iter()
            .map(|pubkey| {
                get_encoded_account(
                    &self.bank, &pubkey, encoding, data_slice, None,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(new_response(&self.bank, accounts))
    }

    pub fn get_program_accounts(
        &self,
        program_id: &Pubkey,
        config: Option<RpcAccountInfoConfig>,
        mut filters: Vec<RpcFilterType>,
        with_context: bool,
    ) -> Result<OptionalContext<Vec<RpcKeyedAccount>>> {
        let RpcAccountInfoConfig {
            encoding,
            data_slice: data_slice_config,
            ..
        } = config.unwrap_or_default();

        let bank = &self.bank;

        let encoding = encoding.unwrap_or(UiAccountEncoding::Binary);

        optimize_filters(&mut filters);

        let keyed_accounts = {
            /* TODO(thlorenz): finish token account support
            if let Some(owner) =
                get_spl_token_owner_filter(program_id, &filters)
            {
                self.get_filtered_spl_token_accounts_by_owner(
                    &bank, program_id, &owner, filters,
                )?
            }
            if let Some(mint) = get_spl_token_mint_filter(program_id, &filters)
            {
                self.get_filtered_spl_token_accounts_by_mint(
                    &bank, program_id, &mint, filters,
                )?
            }
            */
            get_filtered_program_accounts(
                bank,
                program_id,
                &self.config.account_indexes,
                filters,
            )?
        };
        // TODO: possibly JSON parse the accounts

        let accounts = keyed_accounts
            .into_iter()
            .map(|(pubkey, account)| {
                Ok(RpcKeyedAccount {
                    pubkey: pubkey.to_string(),
                    account: encode_account(
                        &account,
                        &pubkey,
                        encoding,
                        data_slice_config,
                    )?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(match with_context {
            true => OptionalContext::Context(new_response(bank, accounts)),
            false => OptionalContext::NoContext(accounts),
        })
    }

    pub fn get_balance(&self, pubkey_str: String) -> Result<RpcResponse<u64>> {
        let pubkey = Pubkey::from_str(&pubkey_str).map_err(|e| Error {
            code: ErrorCode::InvalidParams,
            message: format!("Invalid pubkey: {}", e),
            data: Some(Value::String(pubkey_str)),
        })?;
        let balance = self.bank.get_balance(&pubkey);
        Ok(new_response(&self.bank, balance))
    }

    // -----------------
    // BlockHash
    // -----------------
    pub fn get_latest_blockhash(&self) -> Result<RpcResponse<RpcBlockhash>> {
        let bank = &self.bank;
        let blockhash = bank.last_blockhash();
        let last_valid_block_height = bank
            .get_blockhash_last_valid_block_height(&blockhash)
            .expect("bank blockhash queue should contain blockhash");
        Ok(new_response(
            bank,
            RpcBlockhash {
                blockhash: blockhash.to_string(),
                last_valid_block_height,
            },
        ))
    }

    // -----------------
    // Block
    // -----------------
    pub async fn get_first_available_block(&self) -> Slot {
        // We don't have a blockstore but need to support this request
        0
    }

    pub async fn get_block_time(
        &self,
        slot: Slot,
    ) -> Result<Option<UnixTimestamp>> {
        // Here we differ entirely from the way this is calculated for Solana
        // since for a single node we aren't too worried about clock drift and such.
        // So what we do instead is look at the current time the bank determines and subtract
        // the (duration_slot * (slot - current_slot)) from it.

        let current_slot = self.bank.slot();
        if slot > current_slot {
            // We could predict the timestamp of a future block, but I doubt that makes sens
            Err(Error {
                code: ErrorCode::InvalidRequest,
                message: "Requested slot is in the future".to_string(),
                data: None,
            })
        } else {
            // Expressed as Unix time (i.e. seconds since the Unix epoch).
            let current_time = self.bank.clock().unix_timestamp;
            let slot_diff = current_slot - slot;
            let secs_diff = (slot_diff as u128
                * self.config.slot_duration.as_millis())
                / 1_000;
            let timestamp = current_time - secs_diff as i64;

            Ok(Some(timestamp))
        }
    }

    pub fn get_block_height(&self, config: RpcContextConfig) -> Result<u64> {
        let bank = self.get_bank_with_config(config)?;
        Ok(bank.block_height())
    }

    // -----------------
    // Slot
    // -----------------
    pub fn get_slot(&self, config: RpcContextConfig) -> Result<Slot> {
        let bank = self.get_bank_with_config(config)?;
        Ok(bank.slot())
    }
    // -----------------
    // Bank
    // -----------------
    pub fn get_bank_with_config(
        &self,
        _config: RpcContextConfig,
    ) -> Result<Arc<Bank>> {
        // We only have one bank, so the config isn't important to us
        self.get_bank()
    }

    pub fn get_bank(&self) -> Result<Arc<Bank>> {
        let bank = self.bank.clone();
        Ok(bank)
    }

    pub fn get_transaction_count(
        &self,
        config: RpcContextConfig,
    ) -> Result<u64> {
        let bank = self.get_bank_with_config(config)?;
        Ok(bank.transaction_count())
    }

    pub fn get_supply(
        &self,
        config: Option<RpcSupplyConfig>,
    ) -> RpcCustomResult<RpcResponse<RpcSupply>> {
        let config = config.unwrap_or_default();
        let bank = &self.bank;
        // Our validator doesn't have any accounts that are considered
        // non-circulating. See runtime/src/non_circulating_supply.rs :83
        // We kept the remaining code as intact as possible, but should simplify
        // later once we're sure we won't ever have non-circulating accounts.
        struct NonCirculatingSupply {
            lamports: u64,
            accounts: Vec<Pubkey>,
        }
        let non_circulating_supply = NonCirculatingSupply {
            lamports: 0,
            accounts: vec![],
        };
        let total_supply = bank.capitalization();
        let non_circulating_accounts =
            if config.exclude_non_circulating_accounts_list {
                vec![]
            } else {
                non_circulating_supply
                    .accounts
                    .iter()
                    .map(|pubkey| pubkey.to_string())
                    .collect()
            };

        Ok(new_response(
            bank,
            RpcSupply {
                total: total_supply,
                circulating: total_supply - non_circulating_supply.lamports,
                non_circulating: non_circulating_supply.lamports,
                non_circulating_accounts,
            },
        ))
    }

    // -----------------
    // BankData
    // -----------------
    pub fn get_minimum_balance_for_rent_exemption(
        &self,
        data_len: usize,
    ) -> Result<u64> {
        let bank = &self.bank;

        let balance = bank.get_minimum_balance_for_rent_exemption(data_len);
        Ok(balance)
    }

    pub fn get_epoch_schedule(&self) -> EpochSchedule {
        // Since epoch schedule data comes from the genesis config, any commitment level should be
        // fine
        self.bank.epoch_schedule().clone()
    }

    // -----------------
    // Transactions
    // -----------------
    pub fn request_airdrop(
        &self,
        pubkey_str: String,
        lamports: u64,
    ) -> Result<String> {
        let pubkey = pubkey_str.parse().map_err(|e| Error {
            code: ErrorCode::InvalidParams,
            message: format!("Invalid pubkey: {}", e),
            data: None,
        })?;
        airdrop_transaction(self, pubkey, lamports)
    }

    pub async fn get_transaction(
        &self,
        _signature: Signature,
        config: Option<RpcEncodingConfigWrapper<RpcTransactionConfig>>,
    ) -> Result<Option<EncodedConfirmedTransactionWithStatusMeta>> {
        let config = config
            .map(|config| config.convert_to_current())
            .unwrap_or_default();
        let _encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        // Omit commitment checks

        // TODO(thlorenz): transactions are retrieved either from the blockstore or bigtable ledger
        // storage. We have none of those currently, thus return nothing for now
        // See: rpc/src/rpc.rs :1479

        warn!("get_transaction not yet supported");
        Ok(None)
    }

    pub fn transaction_status_sender(
        &self,
    ) -> Option<&TransactionStatusSender> {
        self.config.transaction_status_sender.as_ref()
    }

    pub async fn get_signature_statuses(
        &self,
        signatures: Vec<Signature>,
        config: Option<RpcSignatureStatusConfig>,
    ) -> Result<RpcResponse<Vec<Option<TransactionStatus>>>> {
        let mut statuses: Vec<Option<TransactionStatus>> = vec![];

        let search_transaction_history = config
            .map(|x| x.search_transaction_history)
            .unwrap_or(false);
        if search_transaction_history
            && !self.config.enable_rpc_transaction_history
        {
            return Err(RpcCustomError::TransactionHistoryNotAvailable.into());
        }
        for signature in signatures {
            let status = self.get_transaction_status(signature);
            // NOTE: we have no blockstore nor bigtable ledger storage to query older transactions
            // from, see: solana/rpc/src/rpc.rs:1436
            statuses.push(status);
        }

        Ok(new_response(&self.bank, statuses))
    }

    fn get_transaction_status(
        &self,
        signature: Signature,
    ) -> Option<TransactionStatus> {
        let (slot, status) = self.bank.get_signature_status_slot(&signature)?;
        let err = status.clone().err();
        Some(TransactionStatus {
            slot,
            status,
            err,
            confirmations: None,
            confirmation_status: None,
        })
    }
}
