use ledger::request::BlockDetails;
use magicblock_core::Slot;
use solana_rpc_client_api::config::RpcBlockConfig;
use solana_transaction_status::{BlockEncodingOptions, TransactionDetails, UiTransactionEncoding};

use super::{HandlerResult, RpcHandlers};
use crate::requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload};

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum BlockConfigParam {
    Config(RpcBlockConfig),
    Encoding(UiTransactionEncoding),
}

impl RpcHandlers {
    pub(crate) async fn get_block(&self, request: &JsonRequest) -> HandlerResult {
        let slot = request.required::<Slot>(0)?;
        let config = match request.optional::<BlockConfigParam>(1)? {
            Some(BlockConfigParam::Config(config)) => config,
            Some(BlockConfigParam::Encoding(encoding)) => RpcBlockConfig {
                encoding: Some(encoding),
                ..RpcBlockConfig::default()
            },
            None => RpcBlockConfig::default(),
        };

        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        let transaction_details = config.transaction_details.unwrap_or_default();
        // Match getTransaction: Aperture can encode every supported version
        // when clients omit the optional version capability.
        let options = BlockEncodingOptions {
            transaction_details,
            show_rewards: config.rewards.unwrap_or(true),
            max_supported_transaction_version: config
                .max_supported_transaction_version
                .or(Some(u8::MAX)),
        };

        let details = match transaction_details {
            TransactionDetails::Full | TransactionDetails::Accounts => BlockDetails::Full,
            TransactionDetails::Signatures => BlockDetails::Signatures,
            TransactionDetails::None => BlockDetails::None,
        };
        let encoded_block = self.history.block(slot, details, encoding, options).await?;
        ResponsePayload::encode_no_context(&request.id, encoded_block)
    }
}
