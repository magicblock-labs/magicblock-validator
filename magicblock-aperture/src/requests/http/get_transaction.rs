use json::{JsonValueMutTrait, JsonValueTrait};
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

        if encoding == UiTransactionEncoding::JsonParsed {
            let mut encoded_value = value_from_serializable(
                &encoded_transaction,
            )
            .ok_or_else(|| {
                RpcError::internal(
                    "failed to serialize JsonParsed getTransaction response",
                )
            })?;
            sanitize_nan_strings(&mut encoded_value);
            return Ok(ResponsePayload::encode_no_context(
                &request.id,
                encoded_value,
            ));
        }

        Ok(ResponsePayload::encode_no_context(
            &request.id,
            encoded_transaction,
        ))
    }
}

fn value_from_serializable<T: json::Serialize>(
    value: &T,
) -> Option<json::Value> {
    json::to_value(value).ok()
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
        if parent_key.is_some_and(is_numeric_amount_field) && is_nan_string(s) {
            *value = "0".into();
        }
    }
}

fn is_numeric_amount_field(key: &str) -> bool {
    matches!(key, "amount" | "uiAmount" | "uiAmountString")
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
                                "uiAmount": "+nan",
                                "uiAmountString": "-nan",
                                "note": "nan"
                            }
                        }
                    }]
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
    }
}
