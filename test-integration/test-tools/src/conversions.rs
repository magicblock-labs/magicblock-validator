use solana_rpc_client_api::{
    client_error, response::RpcSimulateTransactionResult,
};
use solana_sdk::signature::Signature;

pub fn get_rpc_transwise_error_msg(err: &anyhow::Error) -> Option<String> {
    err.source()
        .and_then(|err| err.downcast_ref::<client_error::Error>())
        .and_then(|err| match err.kind() {
            client_error::ErrorKind::RpcError(err) => {
                use solana_rpc_client_api::request::RpcError::*;
                match err {
                    RpcResponseError { message, .. } => Some(message.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
}

pub fn stringify_simulation_result(
    res: RpcSimulateTransactionResult,
    sig: &Signature,
) -> String {
    let mut msg = String::new();
    let error = res.err.map(|e| format!("Error: {:?}", e));
    let logs = res.logs.map(|logs| {
        if logs.is_empty() {
            "".to_string()
        } else {
            format!("{}", logs.join("\n  "))
        }
    });
    let accounts = res.accounts.map_or("".to_string(), |accounts| {
        format!(
            "{:?}",
            accounts
                .into_iter()
                .map(|a| a.map_or("".to_string(), |x| format!("\n{:?}", x)))
                .collect::<Vec<_>>()
        )
    });
    let replacement_blockhash = res
        .replacement_blockhash
        .map(|b| format!("Replacement Blockhash: {:?}", b));

    msg.push_str(format!("Simulation Result: {}\n", sig).as_str());
    if !accounts.is_empty() {
        msg.push('\n');
        msg.push_str("Accounts:");
        msg.push_str(&accounts);
        msg.push('\n');
    }
    if let Some(replacement_blockhash) = replacement_blockhash {
        msg.push('\n');
        msg.push_str(&replacement_blockhash);
        msg.push('\n');
    }
    if let Some(logs) = logs {
        if logs.is_empty() {
            msg.push_str("Logs: <empty>\n");
        } else {
            msg.push_str("Logs:\n  ");
            msg.push_str(&logs);
            msg.push('\n');
        }
    }
    if let Some(error) = error {
        msg.push('\n');
        msg.push_str(&error);
        msg.push('\n');
    }
    msg
}
