use json::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait};
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_transaction_status::UiTransactionEncoding;

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getTransaction` RPC request.
    ///
    /// Fetches the details of a confirmed transaction from the ledger by its
    /// signature. Returns `null` if the transaction is not found.
    pub(crate) fn get_transaction(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (signature, config) = parse_params!(
            request.params()?,
            SerdeSignature,
            RpcTransactionConfig
        );
        let signature = some_or_err!(signature);
        let config = config.unwrap_or_default();

        // Fetch the complete transaction details from the persistent ledger.
        let transaction =
            self.ledger.get_complete_transaction(signature, u64::MAX)?;

        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        // This implementation supports all transaction versions, so we pass a max version number.
        let max_version = Some(u8::MAX);

        // If the transaction was found, encode it for the RPC response.
        let encoded_transaction =
            transaction.and_then(|tx| tx.encode(encoding, max_version).ok());

        let mut encoded_value = value_from_serializable(&encoded_transaction)
            .ok_or_else(|| {
            RpcError::internal("failed to serialize getTransaction response")
        })?;
        normalize_failed_transaction_balance_arrays(&mut encoded_value);

        if encoding == UiTransactionEncoding::JsonParsed {
            sanitize_nan_strings(&mut encoded_value);
        }

        Ok(ResponsePayload::encode_no_context(
            &request.id,
            encoded_value,
        ))
    }
}

fn value_from_serializable<T: json::Serialize>(
    value: &T,
) -> Option<json::Value> {
    json::to_value(value).ok()
}

fn normalize_failed_transaction_balance_arrays(value: &mut json::Value) {
    if value["meta"]["err"].is_null() {
        return;
    }

    let Some(pre_balances) = value["meta"]["preBalances"]
        .as_array()
        .filter(|pre_balances| !pre_balances.is_empty())
        .map(|pre_balances| pre_balances.to_vec())
    else {
        return;
    };

    let post_balance_len = value["meta"]["postBalances"]
        .as_array()
        .map_or(0, |post_balances| post_balances.len());
    if post_balance_len == pre_balances.len() {
        return;
    }

    let mut repaired_post_balances = pre_balances;

    let fee = json_value_as_u64(&value["meta"]["fee"]).unwrap_or(0);
    if let Some(first_balance) = repaired_post_balances.first_mut() {
        if let Some(balance) = json_value_as_u64(first_balance) {
            *first_balance = balance.saturating_sub(fee).into();
        }
    }

    if let Some(encoded_balances) =
        value_from_serializable(&repaired_post_balances)
    {
        value["meta"]["postBalances"] = encoded_balances;
    }
}

fn json_value_as_u64(value: &json::Value) -> Option<u64> {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
        .parse()
        .ok()
}

fn sanitize_nan_strings(value: &mut json::Value) {
    sanitize_nan_strings_for_key(value, None);
}

fn sanitize_nan_strings_for_key(
    value: &mut json::Value,
    parent_key: Option<&str>,
) {
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

    if let Some(s) = value.as_str() {
        if parent_key.is_some_and(is_numeric_json_field) && is_nan_string(s) {
            *value = "0".into();
        }
    }
}

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
            value["transaction"]["message"]["instructions"][0]["parsed"]
                ["info"]["amount"],
            "0"
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]
                ["info"]["lamports"],
            "0"
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]
                ["info"]["microLamports"],
            "0"
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]
                ["info"]["recentSlot"],
            "0"
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]
                ["info"]["uiAmount"],
            "0"
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]
                ["info"]["uiAmountString"],
            "0"
        );
        assert_eq!(
            value["transaction"]["message"]["instructions"][0]["parsed"]
                ["info"]["note"],
            "nan"
        );
        assert_eq!(value["transaction"]["message"]["extra"]["rentEpoch"], "0");
        assert_eq!(value["transaction"]["message"]["extra"]["space"], "0");
        assert_eq!(value["transaction"]["message"]["extra"]["timestamp"], "0");
        assert_eq!(
            value["transaction"]["message"]["extra"]["description"],
            "nan"
        );
    }

    #[test]
    fn normalize_failed_transaction_balance_arrays_repairs_post_balances() {
        let mut value = json::json!({
            "meta": {
                "err": "InvalidWritableAccount",
                "fee": 5000,
                "preBalances": [10000, 20000, 30000],
                "postBalances": []
            }
        });

        normalize_failed_transaction_balance_arrays(&mut value);

        assert_eq!(value["meta"]["postBalances"][0], 5000);
        assert_eq!(value["meta"]["postBalances"][1], 20000);
        assert_eq!(value["meta"]["postBalances"][2], 30000);
    }

    #[test]
    fn normalize_failed_transaction_balance_arrays_keeps_successful_tx() {
        let mut value = json::json!({
            "meta": {
                "err": null,
                "fee": 5000,
                "preBalances": [10000, 20000],
                "postBalances": []
            }
        });

        normalize_failed_transaction_balance_arrays(&mut value);

        assert_eq!(value["meta"]["postBalances"], json::json!([]));
    }
}
