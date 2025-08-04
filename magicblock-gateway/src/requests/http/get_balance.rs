use super::prelude::*;

impl HttpDispatcher {
    pub(crate) async fn get_balance(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let pubkey = parse_params!(request.params()?, Serde32Bytes);
        let pubkey = pubkey.map(Into::into).ok_or_else(|| {
            RpcError::invalid_params("missing or invalid pubkey")
        })?;
        let slot = self.accountsdb.slot();
        let account = self
            .read_account_with_ensure(&pubkey)
            .await
            .map(|a| a.lamports())
            .unwrap_or_default();
        Ok(ResponsePayload::encode(&request.id, account, slot))
    }
}
