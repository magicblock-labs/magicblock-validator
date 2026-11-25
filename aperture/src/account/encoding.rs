//! Account RPC encoding, including the mint metadata needed by parsed token balances.

use std::collections::HashMap;

use accountsdb::AccountLoader;
use engine::Engine;
use serde::Serialize;
use solana_account::{AccountMode, AccountSharedData, ReadableAccount};
use solana_account_decoder::{
    UiAccount, UiAccountData, UiAccountEncoding, UiDataSliceConfig, encode_ui_account,
    parse_account_data::{AccountAdditionalDataV3, SplTokenAdditionalDataV2},
    parse_token::{get_token_account_mint, is_known_spl_token_id},
};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcAccountInfoConfig;
use spl_token_2022::{
    extension::{
        BaseStateWithExtensions, StateWithExtensions, interest_bearing_mint::InterestBearingConfig,
        scaled_ui_amount::ScaledUiAmountConfig,
    },
    state::Mint,
};

use crate::{RpcResult, error::RpcError, requests::params::Serde32Bytes};

pub(crate) struct AccountEncoder {
    encoding: UiAccountEncoding,
    data_slice: Option<UiDataSliceConfig>,
}

impl From<RpcAccountInfoConfig> for AccountEncoder {
    fn from(config: RpcAccountInfoConfig) -> Self {
        Self::new(
            config.encoding.unwrap_or(UiAccountEncoding::Base58),
            config.data_slice,
        )
    }
}

#[derive(Serialize)]
pub(crate) struct KeyedAccount {
    pubkey: Serde32Bytes,
    account: UiAccount,
}

/// The referenced mint and its expected owner. Both identify a validated cache entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TokenMint {
    pub(crate) address: Pubkey,
    program: Pubkey,
}

impl AccountEncoder {
    pub(crate) fn new(encoding: UiAccountEncoding, data_slice: Option<UiDataSliceConfig>) -> Self {
        Self { encoding, data_slice }
    }

    /// Resolve metadata only for parsed token balances. The decoder handles the
    /// binary fallback when a requested account cannot be parsed.
    pub(crate) fn encode(
        &self,
        pubkey: &Pubkey,
        account: &AccountSharedData,
        load_mint: impl FnOnce(TokenMint) -> Option<SplTokenAdditionalDataV2>,
    ) -> UiAccount {
        let metadata = self.token_mint(account).and_then(|mint| mint.resolve(load_mint));
        self.encode_ui(pubkey, account, metadata)
    }

    pub(crate) fn read_visible(
        &self,
        loader: &AccountLoader<'_>,
        pubkey: &Pubkey,
        unix_timestamp: i64,
    ) -> Option<UiAccount> {
        // Reuse the loader's LMDB transaction for the mint instead of opening a nested one.
        loader
            .read(pubkey, |account| {
                // Missing images are storage placeholders, not RPC-visible accounts.
                (!account.is(AccountMode::Missing))
                    .then(|| self.encode(pubkey, account, |mint| mint.read(loader, unix_timestamp)))
            })
            .ok()?
            .flatten()
    }

    pub(crate) fn scan_program(
        &self,
        engine: &Engine,
        program: &Pubkey,
        accepts: impl Fn(&AccountSharedData) -> bool,
    ) -> RpcResult<Vec<KeyedAccount>> {
        let accounts = engine.accounts();
        if !self.parses_token_program(program) {
            let rows = accounts
                .program(program, |pubkey, account| {
                    accepts(account).then(|| KeyedAccount {
                        pubkey: (*pubkey).into(),
                        account: self.encode_ui(pubkey, account, None),
                    })
                })
                .map_err(RpcError::internal)?;
            return Ok(rows.filter_map(|(_, row)| row).collect());
        }

        enum PendingRow {
            Encoded(UiAccount),
            NeedsMint { account: AccountSharedData, mint: TokenMint },
        }

        // The scan owns an LMDB read transaction. Finish it before reading mints,
        // copying only the matched account images that need mint metadata.
        let rows = accounts
            .program(program, |pubkey, account| {
                if !accepts(account) {
                    return None;
                }
                Some(match self.token_mint(account) {
                    Some(mint) => PendingRow::NeedsMint { account: account.clone(), mint },
                    None => PendingRow::Encoded(self.encode_ui(pubkey, account, None)),
                })
            })
            .map_err(RpcError::internal)?
            .filter_map(|(pubkey, row)| Some((pubkey, row?)))
            .collect::<Vec<_>>();

        let loader = accounts.loader();
        let unix_timestamp = engine.blocks().latest().time;
        let mut mint_cache = HashMap::new();
        Ok(rows
            .into_iter()
            .filter_map(|(pubkey, row)| {
                let account = match row {
                    PendingRow::Encoded(account) => account,
                    PendingRow::NeedsMint { account, mint } => {
                        let metadata = mint.resolve(|mint| {
                            *mint_cache
                                .entry(mint)
                                .or_insert_with(|| mint.read(&loader, unix_timestamp))
                        });
                        self.encode_ui(&pubkey, &account, metadata)
                    }
                };
                self.program_row(program, pubkey, account)
            })
            .collect())
    }

    /// Unlike direct reads, parsed token program results omit rows that fall back to binary.
    pub(crate) fn program_row(
        &self,
        program: &Pubkey,
        pubkey: Pubkey,
        account: UiAccount,
    ) -> Option<KeyedAccount> {
        (!self.parses_token_program(program) || matches!(account.data, UiAccountData::Json(_)))
            .then(|| KeyedAccount { pubkey: pubkey.into(), account })
    }

    fn parses_token_program(&self, program: &Pubkey) -> bool {
        self.encoding == UiAccountEncoding::JsonParsed && is_known_spl_token_id(program)
    }

    fn token_mint(&self, account: &AccountSharedData) -> Option<TokenMint> {
        let program = *account.owner();
        if !self.parses_token_program(&program) {
            return None;
        }
        let address = get_token_account_mint(account.data())?;
        Some(TokenMint { address, program })
    }

    fn encode_ui(
        &self,
        pubkey: &Pubkey,
        account: &AccountSharedData,
        mint: Option<SplTokenAdditionalDataV2>,
    ) -> UiAccount {
        let additional_data = mint.map(|mint| AccountAdditionalDataV3 {
            spl_token_additional_data: Some(mint),
        });
        encode_ui_account(
            pubkey,
            account,
            self.encoding,
            additional_data,
            self.data_slice,
        )
    }
}

impl TokenMint {
    fn resolve(
        self,
        load: impl FnOnce(Self) -> Option<SplTokenAdditionalDataV2>,
    ) -> Option<SplTokenAdditionalDataV2> {
        if self.program == spl_token::id() && self.address == spl_token::native_mint::id() {
            return Some(SplTokenAdditionalDataV2::with_decimals(
                spl_token::native_mint::DECIMALS,
            ));
        }
        load(self)
    }

    pub(crate) fn read(
        self,
        loader: &AccountLoader<'_>,
        unix_timestamp: i64,
    ) -> Option<SplTokenAdditionalDataV2> {
        loader
            .read(&self.address, |account| {
                self.decode(account, unix_timestamp)
            })
            .ok()?
            .flatten()
    }

    pub(crate) fn decode(
        self,
        account: &AccountSharedData,
        unix_timestamp: i64,
    ) -> Option<SplTokenAdditionalDataV2> {
        if *account.owner() != self.program {
            return None;
        }
        let mint = StateWithExtensions::<Mint>::unpack(account.data()).ok()?;
        Some(SplTokenAdditionalDataV2 {
            decimals: mint.base.decimals,
            interest_bearing_config: mint
                .get_extension::<InterestBearingConfig>()
                .ok()
                .map(|config| (*config, unix_timestamp)),
            scaled_ui_amount_config: mint
                .get_extension::<ScaledUiAmountConfig>()
                .ok()
                .map(|config| (*config, unix_timestamp)),
        })
    }
}
