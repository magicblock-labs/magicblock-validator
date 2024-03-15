#![allow(dead_code)]
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use crossbeam_channel::{unbounded, Receiver, Sender};
use jsonrpc_core::{Metadata, Result};
use sleipnir_bank::bank::Bank;
use sleipnir_rpc_client_api::{
    config::{
        RpcAccountInfoConfig, RpcContextConfig, UiAccount, UiAccountEncoding,
    },
    filter::RpcFilterType,
    response::{
        OptionalContext, Response as RpcResponse, RpcBlockhash, RpcKeyedAccount,
    },
};
use solana_accounts_db::accounts_index::AccountSecondaryIndexes;
use solana_sdk::{clock::Slot, epoch_schedule::EpochSchedule, pubkey::Pubkey};

use crate::{
    account_resolver::{encode_account, get_encoded_account},
    filters::{get_filtered_program_accounts, optimize_filters},
    rpc_health::RpcHealth,
    utils::new_response,
};

//TODO: send_transaction_service
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
}

// NOTE: from rpc/src/rpc.rs :193
#[derive(Clone)]
pub struct JsonRpcRequestProcessor {
    bank: Arc<Bank>,
    pub(crate) config: JsonRpcConfig,
    transaction_sender: Arc<Mutex<Sender<TransactionInfo>>>,
    pub(crate) health: Arc<RpcHealth>,
}
impl Metadata for JsonRpcRequestProcessor {}

impl JsonRpcRequestProcessor {
    pub fn new(
        bank: Arc<Bank>,
        config: JsonRpcConfig,
        health: Arc<RpcHealth>,
    ) -> (Self, Receiver<TransactionInfo>) {
        let (sender, receiver) = unbounded();
        let transaction_sender = Arc::new(Mutex::new(sender));
        (
            Self {
                bank,
                config,
                transaction_sender,
                health,
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

    // -----------------
    // Epoch
    // -----------------
    pub fn get_epoch_schedule(&self) -> EpochSchedule {
        // Since epoch schedule data comes from the genesis config, any commitment level should be
        // fine
        self.bank.epoch_schedule().clone()
    }
}
