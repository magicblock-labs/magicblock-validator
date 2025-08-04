use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) async fn get_account_info(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (pubkey, config) = parse_params!(
            request.params()?,
            Serde32Bytes,
            RpcAccountInfoConfig
        );
        let pubkey = pubkey.map(Into::into).ok_or_else(|| {
            RpcError::invalid_params("missing or invalid pubkey")
        })?;
        let config = config.unwrap_or_default();
        let slot = self.accountsdb.slot();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let account = self.read_account_with_ensure(&pubkey).await.map(|acc| {
            LockedAccount::new(pubkey, acc).ui_encode(encoding);
        });
        Ok(ResponsePayload::encode(&request.id, account, slot))
    }
}
