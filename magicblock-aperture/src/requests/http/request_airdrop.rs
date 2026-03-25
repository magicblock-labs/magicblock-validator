use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `requestAirdrop` RPC request.
    ///
    /// Creates and processes a system transfer transaction from the validator's
    /// configured faucet account to the specified recipient. Returns an error if
    /// the faucet is not enabled on the node.
    pub(crate) async fn request_airdrop(
        &self,
        _request: &mut JsonRequest,
    ) -> HandlerResult {
        // Airdrops are only supported if a faucet keypair is configured.
        // Which is never the case with *ephemeral* running mode of the validator
        return Err(RpcError::invalid_request(
            "free airdrop faucet is disabled",
        ));
        // TODO(bmuddha): allow free airdrops when other modes are fully reintroduced
        // https://github.com/magicblock-labs/magicblock-validator/issues/1093

        // let (pubkey, lamports) =
        //     parse_params!(request.params()?, Serde32Bytes, u64);
        // let pubkey = some_or_err!(pubkey);
        // let lamports = some_or_err!(lamports);
        // if lamports == 0 {
        //     return Err(RpcError::invalid_params("lamports must be > 0"));
        // }

        // // Build and execute the airdrop transfer transaction.
        // let txn = solana_system_transaction::transfer(
        //     faucet,
        //     &pubkey,
        //     lamports,
        //     self.blocks.get_latest().hash,
        // );
        // // we just signed the transaction, it must have a signature
        // let signature =
        //     SerdeSignature(txn.signatures.first().cloned().unwrap_or_default());

        // self.transactions_scheduler
        //     .execute(with_encoded(txn)?)
        //     .await?;

        // Ok(ResponsePayload::encode_no_context(&request.id, signature))
    }
}
