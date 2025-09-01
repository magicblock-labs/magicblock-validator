use magicblock_core::link::transactions::SanitizeableTransaction;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) async fn request_airdrop(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let Some(ref faucet) = self.context.faucet else {
            return Err(RpcError::invalid_request("method is not supported"));
        };
        let (pubkey, lamports) =
            parse_params!(request.params()?, Serde32Bytes, u64);
        let pubkey = some_or_err!(pubkey);
        let lamports = some_or_err!(lamports);

        let txn = solana_system_transaction::transfer(
            faucet,
            &pubkey,
            lamports,
            self.blocks.get_latest().hash,
        );
        let txn = txn.sanitize()?;
        let signature = *txn.signature();
        self.transactions_scheduler.schedule(txn).await?;
        Ok(ResponsePayload::encode_no_context(&request.id, signature))
    }
}
