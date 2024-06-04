#![allow(dead_code)]
use std::{
    collections::HashMap,
    net::SocketAddr,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use crossbeam_channel::{unbounded, Receiver, Sender};
use jsonrpc_core::{Error, ErrorCode, Metadata, Result, Value};
use log::*;
use sleipnir_accounts::AccountsManager;
use sleipnir_bank::{
    bank::Bank, transaction_simulation::TransactionSimulationResult,
};
use sleipnir_ledger::{Ledger, SignatureInfosForAddress};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_account_decoder::{UiAccount, UiAccountEncoding};
use solana_accounts_db::accounts_index::AccountSecondaryIndexes;
use solana_rpc_client_api::{
    config::{
        RpcAccountInfoConfig, RpcContextConfig, RpcEncodingConfigWrapper,
        RpcSignatureStatusConfig, RpcSimulateTransactionAccountsConfig,
        RpcSupplyConfig, RpcTransactionConfig,
    },
    custom_error::RpcCustomError,
    filter::RpcFilterType,
    response::{
        OptionalContext, Response as RpcResponse, RpcBlockhash,
        RpcConfirmedTransactionStatusWithSignature, RpcContactInfo,
        RpcKeyedAccount, RpcSimulateTransactionResult, RpcSupply,
    },
};
use solana_sdk::{
    clock::{Slot, UnixTimestamp},
    epoch_schedule::EpochSchedule,
    hash::Hash,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    transaction::{
        SanitizedTransaction, TransactionError, VersionedTransaction,
    },
};
use solana_transaction_status::{
    map_inner_instructions, EncodedConfirmedTransactionWithStatusMeta,
    TransactionConfirmationStatus, TransactionStatus, UiInnerInstructions,
    UiTransactionEncoding,
};

use crate::{
    account_resolver::{encode_account, get_encoded_account},
    filters::{get_filtered_program_accounts, optimize_filters},
    rpc_health::{RpcHealth, RpcHealthStatus},
    transaction::{
        airdrop_transaction, ensure_accounts, sanitize_transaction,
        sig_verify_transaction_and_check_precompiles,
    },
    utils::{new_response, verify_pubkey},
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

    /// when the network (bootstrap validator) was started relative to the UNIX Epoch
    pub genesis_creation_time: UnixTimestamp,

    /// Allows updating  Geyser or similar when transactions are processed
    /// Could go into send_transaction_service once we built that
    pub transaction_status_sender: Option<TransactionStatusSender>,
    pub rpc_socket_addr: Option<SocketAddr>,
    pub pubsub_socket_addr: Option<SocketAddr>,

    /// Configures if to verify transaction signatures
    pub disable_sigverify: bool,
}

// NOTE: from rpc/src/rpc.rs :193
#[derive(Clone)]
pub struct JsonRpcRequestProcessor {
    bank: Arc<Bank>,
    pub(crate) ledger: Arc<Ledger>,
    pub(crate) health: Arc<RpcHealth>,
    pub(crate) config: JsonRpcConfig,
    // TODO(thlorenz): lets use this in the future, or deprecate/remove it
    transaction_sender: Arc<Mutex<Sender<TransactionInfo>>>,
    pub(crate) genesis_hash: Hash,
    pub faucet_keypair: Arc<Keypair>,

    pub accounts_manager: Arc<AccountsManager>,
}
impl Metadata for JsonRpcRequestProcessor {}

impl JsonRpcRequestProcessor {
    pub fn new(
        bank: Arc<Bank>,
        ledger: Arc<Ledger>,
        health: Arc<RpcHealth>,
        faucet_keypair: Keypair,
        genesis_hash: Hash,
        accounts_manager: Arc<AccountsManager>,
        config: JsonRpcConfig,
    ) -> (Self, Receiver<TransactionInfo>) {
        let (sender, receiver) = unbounded();
        let transaction_sender = Arc::new(Mutex::new(sender));
        (
            Self {
                bank,
                ledger,
                health,
                config,
                transaction_sender,
                faucet_keypair: Arc::new(faucet_keypair),
                genesis_hash,
                accounts_manager,
            },
            receiver,
        )
    }

    // -----------------
    // Transaction Signatures
    // -----------------
    pub async fn get_signatures_for_address(
        &self,
        address: Pubkey,
        before: Option<Signature>,
        until: Option<Signature>,
        limit: usize,
        config: RpcContextConfig,
    ) -> Result<Vec<RpcConfirmedTransactionStatusWithSignature>> {
        let upper_limit = before;
        let lower_limit = until;

        let highest_slot = {
            let min_context_slot = config.min_context_slot.unwrap_or_default();
            let bank_slot = self.bank.slot();
            if bank_slot < min_context_slot {
                return Err(RpcCustomError::MinContextSlotNotReached {
                    context_slot: bank_slot,
                }
                .into());
            }
            bank_slot
        };

        let SignatureInfosForAddress { infos, .. } = self
            .ledger
            .get_confirmed_signatures_for_address(
                address,
                highest_slot,
                upper_limit,
                lower_limit,
                limit,
            )
            .map_err(|err| Error::invalid_params(format!("{err}")))?;

        // NOTE: we don't support bigtable

        let results = infos
            .into_iter()
            .map(|x| {
                let mut item: RpcConfirmedTransactionStatusWithSignature =
                    x.into();
                // We don't have confirmation status, so we give it the most finalized one
                item.confirmation_status =
                    Some(TransactionConfirmationStatus::Finalized);
                // We assume that the blocktime is always available instead of trying
                // to resolve it via some bank forks (which we don't have)
                item
            })
            .collect();

        Ok(results)
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

    pub fn is_blockhash_valid(
        &self,
        blockhash: &Hash,
        min_context_slot: Option<u64>,
    ) -> Result<RpcResponse<bool>> {
        let bank = self.get_bank();
        let age = match min_context_slot {
            Some(min_slot) => {
                // The original implementation can rely on just the slot to determinine
                // if the min context slot rule applies. It can do that since it can select
                // the appropriate bank for it.
                // In our case we have to estimate this by calculating the age the block hash
                // can have based on the genesis creation time and the slot duration.
                let current_slot = bank.slot();
                if min_slot > current_slot {
                    return Err(Error::invalid_params(format!(
                        "min_context_slot {min_slot} is in the future"
                    )));
                }
                let slot_diff = current_slot - min_slot;
                let slot_diff_millis =
                    (self.config.slot_duration.as_micros() as f64 / 1_000.0
                        * (slot_diff as f64)) as u64;
                let age = slot_diff_millis;
                Some(age)
            }
            None => None,
        };
        let is_valid = match age {
            Some(age) => bank.is_blockhash_valid_for_age(blockhash, age),
            None => bank.is_blockhash_valid(blockhash),
        };

        Ok(new_response(&bank, is_valid))
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

    pub fn get_slot_leaders(
        &self,
        start_slot: Slot,
        limit: usize,
    ) -> Result<Vec<Pubkey>> {
        let slot = self.bank.slot();
        if start_slot > slot {
            return Err(Error::invalid_params(format!(
                "Start slot {start_slot} is in the future; current is {slot}"
            )));
        }

        // We are a single node validator and thus always the leader
        let slot_leader = self.bank.get_identity();
        Ok(vec![slot_leader; limit])
    }

    pub fn get_slot_leader(&self, config: RpcContextConfig) -> Result<Pubkey> {
        let bank = self.get_bank_with_config(config)?;
        Ok(bank.get_identity())
    }

    // -----------------
    // Stats
    // -----------------
    pub fn get_identity(&self) -> Pubkey {
        self.bank.get_identity()
    }

    // -----------------
    // Bank
    // -----------------
    pub fn get_bank_with_config(
        &self,
        _config: RpcContextConfig,
    ) -> Result<Arc<Bank>> {
        // We only have one bank, so the config isn't important to us
        Ok(self.get_bank())
    }

    pub fn get_bank(&self) -> Arc<Bank> {
        self.bank.clone()
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
    pub async fn request_airdrop(
        &self,
        pubkey_str: String,
        lamports: u64,
    ) -> Result<String> {
        let pubkey = pubkey_str.parse().map_err(|e| Error {
            code: ErrorCode::InvalidParams,
            message: format!("Invalid pubkey: {}", e),
            data: None,
        })?;
        airdrop_transaction(
            self,
            pubkey,
            lamports,
            !self.config.disable_sigverify,
        )
        .await
    }

    pub async fn get_transaction(
        &self,
        signature: Signature,
        config: Option<RpcEncodingConfigWrapper<RpcTransactionConfig>>,
    ) -> Result<Option<EncodedConfirmedTransactionWithStatusMeta>> {
        let config = config
            .map(|config| config.convert_to_current())
            .unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        let max_supported_transaction_version =
            config.max_supported_transaction_version.unwrap_or(0);

        // NOTE: Omitting commitment check

        if self.config.enable_rpc_transaction_history {
            let highest_confirmed_slot = self.bank.slot();
            let result = self
                .ledger
                .get_complete_transaction(signature, highest_confirmed_slot);

            // NOTE: not supporting bigtable
            if let Some(tx) = result.ok().flatten() {
                // NOTE: we assume to always have a blocktime
                let encoded = tx
                    .encode(encoding, Some(max_supported_transaction_version))
                    .map_err(RpcCustomError::from)?;
                return Ok(Some(encoded));
            }
        } else {
            return Err(RpcCustomError::TransactionHistoryNotAvailable.into());
        }
        Ok(None)
    }

    pub fn transaction_status_sender(
        &self,
    ) -> Option<&TransactionStatusSender> {
        self.config.transaction_status_sender.as_ref()
    }

    pub fn transaction_preflight(
        &self,
        preflight_bank: &Bank,
        transaction: &SanitizedTransaction,
    ) -> Result<()> {
        match self.health.check() {
            RpcHealthStatus::Ok => (),
            RpcHealthStatus::Unknown => {
                inc_new_counter_info!("rpc-send-tx_health-unknown", 1);
                return Err(RpcCustomError::NodeUnhealthy {
                    num_slots_behind: None,
                }
                .into());
            }
        }

        if let TransactionSimulationResult {
            result: Err(err),
            logs,
            post_simulation_accounts: _,
            units_consumed,
            return_data,
            inner_instructions: _, // Always `None` due to `enable_cpi_recording = false`
        } = preflight_bank.simulate_transaction_unchecked(transaction, false)
        {
            match err {
                TransactionError::BlockhashNotFound => {
                    inc_new_counter_info!(
                        "rpc-send-tx_err-blockhash-not-found",
                        1
                    );
                }
                _ => {
                    inc_new_counter_info!("rpc-send-tx_err-other", 1);
                }
            }
            return Err(RpcCustomError::SendTransactionPreflightFailure {
                message: format!("Transaction simulation failed: {err}"),
                result: RpcSimulateTransactionResult {
                    err: Some(err),
                    logs: Some(logs),
                    accounts: None,
                    units_consumed: Some(units_consumed),
                    return_data: return_data
                        .map(|return_data| return_data.into()),
                    inner_instructions: None,
                },
            }
            .into());
        }

        Ok(())
    }

    pub async fn simulate_transaction(
        &self,
        mut unsanitized_tx: VersionedTransaction,
        config_accounts: Option<RpcSimulateTransactionAccountsConfig>,
        replace_recent_blockhash: bool,
        sig_verify: bool,
        enable_cpi_recording: bool,
    ) -> Result<RpcResponse<RpcSimulateTransactionResult>> {
        let bank = self.get_bank();

        if replace_recent_blockhash {
            if sig_verify {
                return Err(Error::invalid_params(
                    "sigVerify may not be used with replaceRecentBlockhash",
                ));
            }
            unsanitized_tx
                .message
                .set_recent_blockhash(bank.last_blockhash());
        }
        let sanitized_transaction =
            sanitize_transaction(unsanitized_tx, &*bank)?;
        if sig_verify {
            sig_verify_transaction_and_check_precompiles(
                &sanitized_transaction,
                &bank.feature_set,
            )?;
        }

        if let Err(err) =
            ensure_accounts(&self.accounts_manager, &sanitized_transaction)
                .await
        {
            const MAGIC_ID: &str =
                "Magic11111111111111111111111111111111111111";
            let logs = vec![
                format!("{MAGIC_ID}: An error was encountered before simulating the transaction."),
                format!("{MAGIC_ID}: Something went wrong when trying to clone the needed accounts into the validator."),
                format!("{MAGIC_ID}: Error: {err:?}"),
            ];
            return Ok(new_response(
                &bank,
                RpcSimulateTransactionResult {
                    err: Some(TransactionError::AccountNotFound),
                    logs: Some(logs),
                    accounts: None,
                    units_consumed: Some(0),
                    return_data: None,
                    inner_instructions: None,
                },
            ));
        }

        let TransactionSimulationResult {
            result,
            logs,
            post_simulation_accounts,
            units_consumed,
            return_data,
            inner_instructions,
        } = bank.simulate_transaction_unchecked(
            &sanitized_transaction,
            enable_cpi_recording,
        );

        let account_keys = sanitized_transaction.message().account_keys();
        let number_of_accounts = account_keys.len();

        let accounts = if let Some(config_accounts) = config_accounts {
            let accounts_encoding = config_accounts
                .encoding
                .unwrap_or(UiAccountEncoding::Base64);

            if accounts_encoding == UiAccountEncoding::Binary
                || accounts_encoding == UiAccountEncoding::Base58
            {
                return Err(Error::invalid_params(
                    "base58 encoding not supported",
                ));
            }

            if config_accounts.addresses.len() > number_of_accounts {
                return Err(Error::invalid_params(format!(
                    "Too many accounts provided; max {number_of_accounts}"
                )));
            }

            if result.is_err() {
                Some(vec![None; config_accounts.addresses.len()])
            } else {
                let mut post_simulation_accounts_map = HashMap::new();
                for (pubkey, data) in post_simulation_accounts {
                    post_simulation_accounts_map.insert(pubkey, data);
                }

                Some(
                    config_accounts
                        .addresses
                        .iter()
                        .map(|address_str| {
                            let pubkey = verify_pubkey(address_str)?;
                            get_encoded_account(
                                &bank,
                                &pubkey,
                                accounts_encoding,
                                None,
                                Some(&post_simulation_accounts_map),
                            )
                        })
                        .collect::<Result<Vec<_>>>()?,
                )
            }
        } else {
            None
        };

        let inner_instructions = inner_instructions.map(|info| {
            map_inner_instructions(info)
                .map(|converted| {
                    UiInnerInstructions::parse(converted, &account_keys)
                })
                .collect()
        });

        Ok(new_response(
            &bank,
            RpcSimulateTransactionResult {
                err: result.err(),
                logs: Some(logs),
                accounts,
                units_consumed: Some(units_consumed),
                return_data: return_data.map(|return_data| return_data.into()),
                inner_instructions,
            },
        ))
    }

    pub fn get_cluster_nodes(&self) -> Vec<RpcContactInfo> {
        let identity_id = self.bank.get_identity();

        let feature_set = u32::from_le_bytes(
            solana_sdk::feature_set::ID.as_ref()[..4]
                .try_into()
                .unwrap(),
        );
        vec![RpcContactInfo {
            pubkey: identity_id.to_string(),
            gossip: None,
            tpu: None,
            tpu_quic: None,
            rpc: self.config.rpc_socket_addr,
            pubsub: self.config.pubsub_socket_addr,
            version: Some(sleipnir_version::version!().to_string()),
            feature_set: Some(feature_set),
            shred_version: None,
        }]
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
            let status = self
                .get_transaction_status(signature, search_transaction_history);
            statuses.push(status);
        }

        Ok(new_response(&self.bank, statuses))
    }

    fn get_transaction_status(
        &self,
        signature: Signature,
        search_transaction_history: bool,
    ) -> Option<TransactionStatus> {
        let bank_result = self.bank.get_signature_status_slot(&signature);
        let (slot, status) = if let Some(bank_result) = bank_result {
            bank_result
        } else if self.config.enable_rpc_transaction_history
            && search_transaction_history
        {
            match self
                .ledger
                .get_transaction_status(signature, self.bank.slot())
            {
                Ok(Some((slot, status))) => (slot, status.status),
                Err(err) => {
                    warn!(
                        "Error loading signature {} from ledger: {:?}",
                        signature, err
                    );
                    return None;
                }
                _ => return None,
            }
        } else {
            return None;
        };
        let err = status.clone().err();
        Some(TransactionStatus {
            slot,
            status,
            err,
            confirmations: None,
            confirmation_status: Some(TransactionConfirmationStatus::Finalized),
        })
    }
}
