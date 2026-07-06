use std::collections::HashSet;

use ledger::request::AccountSignaturesParams;
use solana_rpc_client_api::response::RpcConfirmedTransactionStatusWithSignature;
use solana_transaction_status::{
    ConfirmedTransactionStatusWithSignature, TransactionConfirmationStatus,
};

use super::prelude::*;

const DEFAULT_SIGNATURES_LIMIT: usize = 1_000;

impl HttpDispatcher {
    /// Handles `getSignaturesForAddress` by merging the engine's retained
    /// history with older deprecated-ledger entries.
    pub(crate) async fn get_signatures_for_address(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
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
        let before = config.before.map(Into::into);
        let until = config.until.map(Into::into);

        // A cursor retained by the engine partitions the history: legacy data
        // is older than an engine `before` cursor and newer engine data is
        // excluded by a legacy `before` cursor. The same boundary is inverted
        // for `until`.
        let before_in_engine = if let Some(signature) = before {
            self.engine
                .transactions()
                .status(signature)
                .await
                .map_err(RpcError::internal)?
                .is_some()
        } else {
            false
        };
        let until_in_engine = if let Some(signature) = until {
            self.engine
                .transactions()
                .status(signature)
                .await
                .map_err(RpcError::internal)?
                .is_some()
        } else {
            false
        };

        let include_engine = before.is_none() || before_in_engine;
        let engine = if include_engine {
            self.engine
                .accounts()
                .signatures(AccountSignaturesParams {
                    pubkey: address,
                    limit,
                    before: before_in_engine.then_some(before).flatten(),
                    until: until_in_engine.then_some(until).flatten(),
                })
                .await
                .map_err(RpcError::internal)?
        } else {
            Vec::new()
        };

        let include_legacy = !until_in_engine;
        let legacy = if include_legacy {
            self.ledger
                .get_confirmed_signatures_for_address(
                    address,
                    Slot::MAX,
                    (!before_in_engine).then_some(before).flatten(),
                    (!until_in_engine).then_some(until).flatten(),
                    limit,
                )?
                .infos
        } else {
            Vec::new()
        };

        let mut merged = engine
            .into_iter()
            .map(|info| {
                (
                    ConfirmedTransactionStatusWithSignature {
                        signature: info.signature,
                        slot: info.slot,
                        err: info.result.err(),
                        memo: None,
                        block_time: (info.blocktime != 0)
                            .then_some(info.blocktime),
                        // The engine does not retain an intra-block transaction index.
                        index: 0,
                    },
                    true,
                )
            })
            .chain(legacy.into_iter().map(|info| (info, false)))
            .collect::<Vec<_>>();
        merged.sort_by(|a, b| {
            b.0.slot
                .cmp(&a.0.slot)
                .then_with(|| b.0.index.cmp(&a.0.index))
        });
        let mut seen = HashSet::with_capacity(merged.len());
        merged.retain(|info| seen.insert(info.0.signature));
        merged.truncate(limit);

        let signatures = merged
            .into_iter()
            .map(|(info, from_engine)| {
                let mut rpc =
                    RpcConfirmedTransactionStatusWithSignature::from(info);
                rpc.confirmation_status =
                    Some(TransactionConfirmationStatus::Finalized);
                if from_engine {
                    // Preserve the documented engine placeholder instead of
                    // presenting a fabricated transaction index.
                    rpc.transaction_index = None;
                }
                rpc
            })
            .collect::<Vec<_>>();

        Ok(ResponsePayload::encode_no_context(&request.id, signatures))
    }
}
