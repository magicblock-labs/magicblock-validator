use magicblock_core::link::transactions::SanitizeableTransaction;

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `requestAirdrop` RPC request.
    ///
    /// Creates and processes a system transfer transaction from the validator's
    /// configured faucet account to the specified recipient. Returns an error if
    /// the faucet is not enabled on the node.
    pub(crate) async fn request_airdrop(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        // Airdrops are only supported if a faucet keypair is configured.
        // Which is never the case with *ephemeral* running mode of the validator
        let Some(ref faucet) = self.context.faucet else {
            return Err(RpcError::invalid_request("method is not supported"));
        };

        let (pubkey, lamports) =
            parse_params!(request.params()?, Serde32Bytes, u64);
        let pubkey = some_or_err!(pubkey);
        let lamports = some_or_err!(lamports);

        // Build and execute the airdrop transfer transaction.
        let txn = solana_system_transaction::transfer(
            faucet,
            &pubkey,
            lamports,
            self.blocks.get_latest().hash,
        );
        let txn = txn.sanitize()?;
        let signature = SerdeSignature(*txn.signature());

        self.transactions_scheduler.execute(txn).await?;

        Ok(ResponsePayload::encode_no_context(&request.id, signature))
    }
}
