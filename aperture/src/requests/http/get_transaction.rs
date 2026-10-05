use json::{JsonValueMutTrait, JsonValueTrait};
use solana_rpc_client_api::config::{RpcEncodingConfigWrapper, RpcTransactionConfig};
use solana_transaction_status::{ConfirmedTransactionWithStatusMeta, UiTransactionEncoding};

use super::{HandlerResult, RpcHandlers};
use crate::{
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, params::SerdeSignature, payload::ResponsePayload},
};

impl RpcHandlers {
    pub(crate) async fn get_transaction(&self, request: &JsonRequest) -> HandlerResult {
        let signature = request.required::<SerdeSignature>(0)?.into();
        let config = request
            .optional::<RpcEncodingConfigWrapper<RpcTransactionConfig>>(1)?
            .map(|config| config.convert_to_current())
            .unwrap_or_default();

        let encode = |transaction: Option<ConfirmedTransactionWithStatusMeta>| {
            let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
            // This implementation supports all transaction versions, so we pass a max version number.
            let max_version = Some(u8::MAX);

            // If the transaction was found, encode it for the RPC response.
            let encoded_transaction =
                transaction.and_then(|tx| tx.encode(encoding, max_version).ok());

            if encoding != UiTransactionEncoding::JsonParsed {
                return ResponsePayload::encode_no_context(&request.id, encoded_transaction);
            }
            let mut encoded_value =
                json::to_value(&encoded_transaction).map_err(RpcError::internal)?;
            sanitize_nan_strings(&mut encoded_value);
            ResponsePayload::encode_no_context(&request.id, encoded_value)
        };

        self.history.transaction(signature, encode).await
    }
}

/// Normalizes parser-produced NaN strings only in known numeric fields.
fn sanitize_nan_strings(value: &mut json::Value) {
    sanitize_nan_strings_for_key(value, None);
}

/// Walks parsed transaction fields while retaining the containing key's semantics.
fn sanitize_nan_strings_for_key(value: &mut json::Value, parent_key: Option<&str>) {
    if let Some(values) = value.as_array_mut() {
        for value in values {
            sanitize_nan_strings_for_key(value, parent_key);
        }
        return;
    }

    if let Some(values) = value.as_object_mut() {
        for (key, value) in values.iter_mut() {
            sanitize_nan_strings_for_key(value, Some(key));
        }
        return;
    }

    if let Some(s) = value.as_str()
        && let Some(key) = parent_key.filter(|key| is_numeric_json_field(key))
        && is_nan_string(s)
    {
        *value = nan_replacement_for_field(key);
    }
}

/// Keeps string amounts as strings and replaces other numeric NaNs with zero.
fn nan_replacement_for_field(key: &str) -> json::Value {
    match key {
        "amount" | "uiAmountString" => "0".into(),
        _ => 0.into(),
    }
}

/// Limits normalization so logs, memos, and arbitrary string data remain unchanged.
fn is_numeric_json_field(key: &str) -> bool {
    matches!(
        key,
        "accountDataSizeLimit"
            | "activationEpoch"
            | "additionalFee"
            | "amount"
            | "bytes"
            | "commission"
            | "computeUnitsConsumed"
            | "costUnits"
            | "deactivationEpoch"
            | "epoch"
            | "fee"
            | "lamports"
            | "microLamports"
            | "recentSlot"
            | "rentEpoch"
            | "rentExemptReserve"
            | "space"
            | "stake"
            | "timestamp"
            | "uiAmount"
            | "uiAmountString"
            | "unixTimestamp"
            | "units"
    )
}

/// Recognizes the parser's signed, case-insensitive NaN spellings.
fn is_nan_string(value: &str) -> bool {
    value.eq_ignore_ascii_case("nan")
        || value.eq_ignore_ascii_case("+nan")
        || value.eq_ignore_ascii_case("-nan")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_nan_strings_only_changes_numeric_amount_fields() {
        let mut value = json::json!({
            "meta": {
                "logMessages": ["nan", "+nan", "-nan"],
                "memo": "nan"
            },
            "transaction": {
                "message": {
                    "instructions": [{
                        "parsed": {
                            "info": {
                                "amount": "nan",
                                "lamports": "nan",
                                "microLamports": "+nan",
                                "recentSlot": "-nan",
                                "uiAmount": "+nan",
                                "uiAmountString": "-nan",
                                "note": "nan"
                            }
                        }
                    }],
                    "extra": {
                        "rentEpoch": "nan",
                        "space": "+nan",
                        "timestamp": "-nan",
                        "description": "nan"
                    }
                }
            }
        });

        sanitize_nan_strings(&mut value);

        assert_eq!(value["meta"]["logMessages"][0], "nan");
        assert_eq!(value["meta"]["logMessages"][1], "+nan");
        assert_eq!(value["meta"]["logMessages"][2], "-nan");
        assert_eq!(value["meta"]["memo"], "nan");
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]["info"]["amount"],
            "0"
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]["info"]["lamports"],
            0
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]["info"]["microLamports"],
            0
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]["info"]["recentSlot"],
            0
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]["info"]["uiAmount"],
            0
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]["info"]["uiAmountString"],
            "0"
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]["info"]["note"],
            "nan"
        );
        assert_eq!(value["transaction"]["message"]["extra"]["rentEpoch"], 0);
        assert_eq!(value["transaction"]["message"]["extra"]["space"], 0);
        assert_eq!(value["transaction"]["message"]["extra"]["timestamp"], 0);
        assert_eq!(
            value["transaction"]["message"]["extra"]["description"],
            "nan"
        );
    }
}
