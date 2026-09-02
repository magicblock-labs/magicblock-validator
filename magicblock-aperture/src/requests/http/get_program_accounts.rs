use json::Serialize;
use solana_account::{AccountSeqLock, AccountSharedData, ReadableAccount};
use solana_account_decoder::{
    UiAccount, UiAccountEncoding, UiDataSliceConfig, encode_ui_account,
};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::RpcProgramAccountsConfig, filter::RpcFilterType,
};
use spl_token_2022::{
    generic_token_account::GenericTokenAccount, state::Account as TokenAccount,
};

use super::HandlerResult;
use crate::{
    error::RpcError,
    requests::{
        JsonHttpRequest as JsonRequest, params::Serde32Bytes,
        payload::ResponsePayload,
    },
    server::http::dispatch::HttpDispatcher,
};

#[derive(Serialize)]
pub(crate) struct AccountWithPubkey {
    pubkey: Serde32Bytes,
    account: UiAccount,
}

impl AccountWithPubkey {
    pub(crate) fn new(
        pubkey: Pubkey,
        account: &AccountSharedData,
        encoding: UiAccountEncoding,
        slice: Option<UiDataSliceConfig>,
    ) -> Self {
        Self {
            pubkey: pubkey.into(),
            account: encode_ui_account(&pubkey, account, encoding, None, slice),
        }
    }
}

pub(crate) fn matches_filters(filters: &[RpcFilterType], data: &[u8]) -> bool {
    filters.iter().all(|filter| match filter {
        RpcFilterType::DataSize(size) => data.len() as u64 == *size,
        RpcFilterType::Memcmp(memcmp) => memcmp.bytes_match(data),
        RpcFilterType::TokenAccountState => {
            TokenAccount::valid_account_data(data)
        }
    })
}

impl HttpDispatcher {
    pub(crate) fn get_program_accounts(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let program: Pubkey = request.required::<Serde32Bytes>(0)?.into();
        let config = request
            .optional::<RpcProgramAccountsConfig>(1)?
            .unwrap_or_default();
        let filters = config.filters.unwrap_or_default();
        for filter in &filters {
            filter.verify().map_err(RpcError::invalid_params)?;
        }

        let encoding = config
            .account_config
            .encoding
            .unwrap_or(UiAccountEncoding::Base58);
        let slice = config.account_config.data_slice;

        let accounts = self.engine.accounts();
        let accounts = accounts
            .program(&program)
            .map_err(RpcError::internal)?
            .filter_map(|(pubkey, account)| {
                AccountSeqLock::new(account).read(|account| {
                    matches_filters(&filters, account.data()).then(|| {
                        AccountWithPubkey::new(pubkey, account, encoding, slice)
                    })
                })
            })
            .collect::<Vec<_>>();

        if config.with_context.unwrap_or_default() {
            let slot = self.engine.blocks().latest().slot;
            Ok(ResponsePayload::encode(&request.id, accounts, slot))
        } else {
            Ok(ResponsePayload::encode_no_context(&request.id, accounts))
        }
    }
}
