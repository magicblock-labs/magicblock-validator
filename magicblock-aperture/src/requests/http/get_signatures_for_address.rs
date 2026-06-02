use magicblock_metrics::metrics::AccountFetchOrigin;
use solana_rpc_client_api::response::RpcConfirmedTransactionStatusWithSignature;
use solana_transaction_status::TransactionConfirmationStatus;

use super::prelude::*;

const DEFAULT_SIGNATURES_LIMIT: usize = 1_000;

impl HttpDispatcher {
    /// Handles the `getSignaturesForAddress` RPC request.
    ///
    /// Fetches a list of confirmed transaction signatures for a given address,
    /// sorted in reverse chronological order. The query can be paginated using
    /// the optional `limit`, `before`, and `until` parameters.
    pub(crate) async fn get_signatures_for_address(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        /// A helper struct for deserializing the optional configuration
        /// object for the `getSignaturesForAddress` request.
        #[derive(serde::Deserialize, Default)]
        #[serde(rename_all = "camelCase")]
        struct Config {
            until: Option<SerdeSignature>,
            before: Option<SerdeSignature>,
            limit: Option<usize>,
        }

        let (address, config) =
            parse_params!(request.params()?, Serde32Bytes, Config);
        let address = some_or_err!(address);
        let config = config.unwrap_or_default();

        let limit = config
            .limit
            .unwrap_or(DEFAULT_SIGNATURES_LIMIT)
            .min(DEFAULT_SIGNATURES_LIMIT);
        let signatures_result =
            self.ledger.get_confirmed_signatures_for_address(
                address,
                Slot::MAX,
                config.before.map(Into::into),
                config.until.map(Into::into),
                limit,
            )?;

        let mut signatures = signatures_result
            .infos
            .into_iter()
            .map(|info| {
                let mut rpc_status =
                    RpcConfirmedTransactionStatusWithSignature::from(info);
                // This validator considers all transactions in the ledger to be finalized.
                rpc_status
                    .confirmation_status
                    .replace(TransactionConfirmationStatus::Finalized);
                rpc_status
            })
            .collect::<Vec<_>>();
        if let Some(user) = &request.authenticated_user {
            self.ensure_permission_accounts(
                &[address],
                AccountFetchOrigin::GetAccount,
            )
            .await?;
            let permission =
                magicblock_query_filtering::permission_for_account(
                    &*self.accountsdb,
                    &address,
                )
                .map_err(RpcError::internal)?;
            signatures = magicblock_query_filtering::filter_signatures(
                signatures,
                &permission,
                user,
            );
        }

        Ok(ResponsePayload::encode_no_context(&request.id, signatures))
    }
}
