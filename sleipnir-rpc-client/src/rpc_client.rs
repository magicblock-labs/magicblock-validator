// NOTE: adapted from rpc-client/src/nonblocking/rpc_client.rs
use log::trace;
use serde_json::{json, Value};
use sleipnir_rpc_client_api::{
    client_error::{Error as ClientError, Result as ClientResult},
    config::{RpcAccountInfoConfig, UiAccount, UiAccountEncoding},
    request::{RpcError, RpcRequest},
    response::{Response, RpcResult},
};
use solana_sdk::{account::Account, commitment_config::CommitmentConfig, pubkey::Pubkey};
use std::{sync::RwLock, time::Duration};

use crate::{http_sender::HttpSender, rpc_sender::RpcSender};

// -----------------
// RpcClientConfig
// -----------------
#[derive(Default)]
pub struct RpcClientConfig {
    pub commitment_config: CommitmentConfig,
    pub confirm_transaction_initial_timeout: Option<Duration>,
}

impl RpcClientConfig {
    pub fn with_commitment(commitment_config: CommitmentConfig) -> Self {
        RpcClientConfig {
            commitment_config,
            ..Self::default()
        }
    }
}

// -----------------
// RpcClient
// -----------------
pub struct RpcClient {
    sender: Box<dyn RpcSender + Send + Sync + 'static>,
    config: RpcClientConfig,
    #[allow(dead_code)]
    node_version: RwLock<Option<semver::Version>>,
}

impl RpcClient {
    /// Create an `RpcClient` from an [`RpcSender`] and an [`RpcClientConfig`].
    ///
    /// This is the basic constructor, allowing construction with any type of
    /// `RpcSender`. Most applications should use one of the other constructors,
    /// such as [`RpcClient::new`], [`RpcClient::new_with_commitment`] or
    /// [`RpcClient::new_with_timeout`].
    pub fn new_sender<T: RpcSender + Send + Sync + 'static>(
        sender: T,
        config: RpcClientConfig,
    ) -> Self {
        Self {
            sender: Box::new(sender),
            node_version: RwLock::new(None),
            config,
        }
    }

    /// Create an HTTP `RpcClient` with specified [commitment level][cl].
    ///
    /// [cl]: https://solana.com/docs/rpc#configuring-state-commitment
    ///
    /// The URL is an HTTP URL, usually for port 8899, as in
    /// "http://localhost:8899".
    ///
    /// The client has a default timeout of 30 seconds, and a user-specified
    /// [`CommitmentLevel`] via [`CommitmentConfig`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use solana_sdk::commitment_config::CommitmentConfig;
    /// # use sleipnir_rpc_client::rpc_client::RpcClient;
    /// let url = "http://localhost:8899".to_string();
    /// let commitment_config = CommitmentConfig::processed();
    /// let client = RpcClient::new_with_commitment(url, commitment_config);
    /// ```
    pub fn new_with_commitment(url: String, commitment_config: CommitmentConfig) -> Self {
        Self::new_sender(
            HttpSender::new(url),
            RpcClientConfig::with_commitment(commitment_config),
        )
    }

    /// Returns all information associated with the account of the provided pubkey.
    ///
    /// This method uses the configured [commitment level][cl].
    ///
    /// [cl]: https://solana.com/docs/rpc#configuring-state-commitment
    ///
    /// To get multiple accounts at once, use the [`get_multiple_accounts`] method.
    ///
    /// [`get_multiple_accounts`]: RpcClient::get_multiple_accounts
    ///
    /// # Errors
    ///
    /// If the account does not exist, this method returns
    /// [`RpcError::ForUser`]. This is unlike [`get_account_with_commitment`],
    /// which returns `Ok(None)` if the account does not exist.
    ///
    /// [`get_account_with_commitment`]: RpcClient::get_account_with_commitment
    ///
    /// # RPC Reference
    ///
    /// This method is built on the [`getAccountInfo`] RPC method.
    ///
    /// [`getAccountInfo`]: https://solana.com/docs/rpc/http/getaccountinfo
    ///
    /// # Examples
    ///
    /// ```
    /// # use sleipnir_rpc_client_api::client_error::Error;
    /// # use sleipnir_rpc_client::rpc_client::{self, RpcClient};
    /// # use solana_sdk::{
    /// #     signature::Signer,
    /// #     signer::keypair::Keypair,
    /// #     pubkey::Pubkey,
    /// # };
    /// # use std::str::FromStr;
    /// # futures::executor::block_on(async {
    /// #     let mocks = rpc_client::create_rpc_client_mocks();
    /// #     let rpc_client = RpcClient::new_mock_with_mocks("succeeds".to_string(), mocks);
    /// let alice_pubkey = Pubkey::from_str("BgvYtJEfmZYdVKiptmMjxGzv8iQoo4MWjsP3QsTkhhxa").unwrap();
    /// let account = rpc_client.get_account(&alice_pubkey).await?;
    /// #     Ok::<(), Error>(())
    /// # })?;
    /// # Ok::<(), Error>(())
    /// ```
    pub async fn get_account(&self, pubkey: &Pubkey) -> ClientResult<Account> {
        self.get_account_with_commitment(pubkey, self.commitment())
            .await?
            .value
            .ok_or_else(|| RpcError::ForUser(format!("AccountNotFound: pubkey={pubkey}")).into())
    }

    /// Returns all information associated with the account of the provided pubkey.
    ///
    /// If the account does not exist, this method returns `Ok(None)`.
    ///
    /// To get multiple accounts at once, use the [`get_multiple_accounts_with_commitment`] method.
    ///
    /// [`get_multiple_accounts_with_commitment`]: RpcClient::get_multiple_accounts_with_commitment
    ///
    /// # RPC Reference
    ///
    /// This method is built on the [`getAccountInfo`] RPC method.
    ///
    /// [`getAccountInfo`]: https://solana.com/docs/rpc/http/getaccountinfo
    ///
    /// # Examples
    ///
    /// ```
    /// # use sleipnir_rpc_client_api::client_error::Error;
    /// # use sleipnir_rpc_client::rpc_client::{self, RpcClient};
    /// # use solana_sdk::{
    /// #     signature::Signer,
    /// #     signer::keypair::Keypair,
    /// #     pubkey::Pubkey,
    /// #     commitment_config::CommitmentConfig,
    /// # };
    /// # use std::str::FromStr;
    /// # futures::executor::block_on(async {
    /// #     let mocks = rpc_client::create_rpc_client_mocks();
    /// #     let rpc_client = RpcClient::new_mock_with_mocks("succeeds".to_string(), mocks);
    /// let alice_pubkey = Pubkey::from_str("BgvYtJEfmZYdVKiptmMjxGzv8iQoo4MWjsP3QsTkhhxa").unwrap();
    /// let commitment_config = CommitmentConfig::processed();
    /// let account = rpc_client.get_account_with_commitment(
    ///     &alice_pubkey,
    ///     commitment_config,
    /// ).await?;
    /// assert!(account.value.is_some());
    /// #     Ok::<(), Error>(())
    /// # })?;
    /// # Ok::<(), Error>(())
    /// ```
    pub async fn get_account_with_commitment(
        &self,
        pubkey: &Pubkey,
        commitment_config: CommitmentConfig,
    ) -> RpcResult<Option<Account>> {
        let config = RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64Zstd),
            commitment: Some(commitment_config),
            data_slice: None,
            min_context_slot: None,
        };

        self.get_account_with_config(pubkey, config).await
    }

    /// Returns all information associated with the account of the provided pubkey.
    ///
    /// If the account does not exist, this method returns `Ok(None)`.
    ///
    /// To get multiple accounts at once, use the [`get_multiple_accounts_with_config`] method.
    ///
    /// [`get_multiple_accounts_with_config`]: RpcClient::get_multiple_accounts_with_config
    ///
    /// # RPC Reference
    ///
    /// This method is built on the [`getAccountInfo`] RPC method.
    ///
    /// [`getAccountInfo`]: https://solana.com/docs/rpc/http/getaccountinfo
    ///
    /// # Examples
    ///
    /// ```
    /// # use sleipnir_rpc_client_api::{
    /// #     config::RpcAccountInfoConfig,
    /// #     client_error::Error,
    /// # };
    /// # use sleipnir_rpc_client::rpc_client::{self, RpcClient};
    /// # use solana_sdk::{
    /// #     signature::Signer,
    /// #     signer::keypair::Keypair,
    /// #     pubkey::Pubkey,
    /// #     commitment_config::CommitmentConfig,
    /// # };
    /// # use solana_account_decoder::UiAccountEncoding;
    /// # use std::str::FromStr;
    /// # futures::executor::block_on(async {
    /// #     let mocks = rpc_client::create_rpc_client_mocks();
    /// #     let rpc_client = RpcClient::new_mock_with_mocks("succeeds".to_string(), mocks);
    /// let alice_pubkey = Pubkey::from_str("BgvYtJEfmZYdVKiptmMjxGzv8iQoo4MWjsP3QsTkhhxa").unwrap();
    /// let commitment_config = CommitmentConfig::processed();
    /// let config = RpcAccountInfoConfig {
    ///     encoding: Some(UiAccountEncoding::Base64),
    ///     commitment: Some(commitment_config),
    ///     .. RpcAccountInfoConfig::default()
    /// };
    /// let account = rpc_client.get_account_with_config(
    ///     &alice_pubkey,
    ///     config,
    /// ).await?;
    /// assert!(account.value.is_some());
    /// #     Ok::<(), Error>(())
    /// # })?;
    /// # Ok::<(), Error>(())
    /// ```
    pub async fn get_account_with_config(
        &self,
        pubkey: &Pubkey,
        config: RpcAccountInfoConfig,
    ) -> RpcResult<Option<Account>> {
        let response = self
            .send(
                RpcRequest::GetAccountInfo,
                json!([pubkey.to_string(), config]),
            )
            .await;

        response
            .map(|result_json: Value| {
                if result_json.is_null() {
                    return Err(
                        RpcError::ForUser(format!("AccountNotFound: pubkey={pubkey}")).into(),
                    );
                }
                let Response {
                    context,
                    value: rpc_account,
                } = serde_json::from_value::<Response<Option<UiAccount>>>(result_json)?;
                trace!("Response account {:?} {:?}", pubkey, rpc_account);
                let account = rpc_account.and_then(|rpc_account| rpc_account.decode());

                Ok(Response {
                    context,
                    value: account,
                })
            })
            .map_err(|err| {
                Into::<ClientError>::into(RpcError::ForUser(format!(
                    "AccountNotFound: pubkey={pubkey}: {err}"
                )))
            })?
    }

    /// Get the configured default [commitment level][cl].
    ///
    /// [cl]: https://solana.com/docs/rpc#configuring-state-commitment
    ///
    /// The commitment config may be specified during construction, and
    /// determines how thoroughly committed a transaction must be when waiting
    /// for its confirmation or otherwise checking for confirmation. If not
    /// specified, the default commitment level is
    /// [`Finalized`](CommitmentLevel::Finalized).
    ///
    /// The default commitment level is overridden when calling methods that
    /// explicitly provide a [`CommitmentConfig`], like
    /// [`RpcClient::confirm_transaction_with_commitment`].
    pub fn commitment(&self) -> CommitmentConfig {
        self.config.commitment_config
    }

    pub async fn send<T>(&self, request: RpcRequest, params: Value) -> ClientResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        assert!(params.is_array() || params.is_null());

        let response = self
            .sender
            .send(request, params)
            .await
            .map_err(|err| err.into_with_request(request))?;
        serde_json::from_value(response)
            .map_err(|err| ClientError::new_with_request(err.into(), request))
    }
}
