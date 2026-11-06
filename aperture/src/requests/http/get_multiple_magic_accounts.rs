use crate::account::Accounts;
use solana_account::{AccountMode, AccountSharedData, StateFlags};
use solana_account_decoder::{UiAccountEncoding, encode_ui_account};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{config::RpcAccountInfoConfig, request::MAX_MULTIPLE_ACCOUNTS};

use super::{HandlerResult, RpcHandlers};
use crate::{
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, params::Serde32Bytes, payload::ResponsePayload},
};

impl RpcHandlers {
    /// Same shape as `getMultipleAccounts`, but only rent-funded Magic images
    /// already stored locally. Absent and non-ephemeral keys are null.
    pub(crate) fn get_multiple_magic_accounts(&self, request: &JsonRequest) -> HandlerResult {
        let pubkeys = request.required::<Vec<Serde32Bytes>>(0)?;
        if pubkeys.len() > MAX_MULTIPLE_ACCOUNTS {
            return Err(RpcError::invalid_params(format!(
                "Too many accounts provided; max {MAX_MULTIPLE_ACCOUNTS}"
            )));
        }
        let pubkeys: Vec<Pubkey> = pubkeys.into_iter().map(Into::into).collect();

        let config = request.optional::<RpcAccountInfoConfig>(1)?.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let slice = config.data_slice;

        let accessor = self.engine.accounts();
        let loader = accessor.loader();
        let accounts = pubkeys
            .iter()
            .map(|pubkey| {
                loader
                    .read(pubkey, |account| {
                        (Accounts::account_is_visible(account) && is_ephemeral(account))
                            .then(|| encode_ui_account(pubkey, account, encoding, None, slice))
                    })
                    .ok()
                    .flatten()
                    .flatten()
            })
            .collect::<Vec<_>>();

        let slot = self.engine.blocks().latest().slot;
        ResponsePayload::encode(&request.id, accounts, slot)
    }
}

/// Ephemeral accounts are Magic images whose data is backed by a rent deposit.
fn is_ephemeral(account: &AccountSharedData) -> bool {
    account.is(AccountMode::Magic) && account.flags().contains(StateFlags::RENT_FUNDED)
}
