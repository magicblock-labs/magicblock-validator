use std::sync::Arc;

use magicblock_committor_service::{
    error::{CommittorServiceError, CommittorServiceResult},
    BaseIntentCommittor,
};
use thiserror::Error;
use tokio::sync::oneshot;

pub type AccountClonerResult<T> = Result<T, AccountClonerError>;

#[derive(Debug, Clone, Error)]
pub enum AccountClonerError {
    #[error(transparent)]
    RecvError(#[from] tokio::sync::oneshot::error::RecvError),

    #[error("JoinError ({0})")]
    JoinError(String),

    #[error("CommittorServiceError {0}")]
    CommittorServiceError(String),
}

pub async fn map_committor_request_result<T, CC: BaseIntentCommittor>(
    res: oneshot::Receiver<CommittorServiceResult<T>>,
    intent_committor: Arc<CC>,
) -> AccountClonerResult<T> {
    match res.await.map_err(|err| {
        // Send request error
        AccountClonerError::CommittorServiceError(format!(
            "error sending request {err:?}"
        ))
    })? {
        Ok(val) => Ok(val),
        Err(err) => {
            // Commit error
            match err {
                CommittorServiceError::TableManiaError(table_mania_err) => {
                    let Some(sig) = table_mania_err.signature() else {
                        return Err(AccountClonerError::CommittorServiceError(
                            format!("{:?}", table_mania_err),
                        ));
                    };
                    let (logs, cus) = crate::util::get_tx_diagnostics(
                        &sig,
                        &intent_committor,
                    )
                    .await;

                    let cus_str = cus
                        .map(|cus| format!("{:?}", cus))
                        .unwrap_or("N/A".to_string());
                    let logs_str = logs
                        .map(|logs| format!("{:#?}", logs))
                        .unwrap_or("N/A".to_string());
                    Err(AccountClonerError::CommittorServiceError(format!(
                        "{:?}\nCUs: {cus_str}\nLogs: {logs_str}",
                        table_mania_err
                    )))
                }
                _ => Err(AccountClonerError::CommittorServiceError(format!(
                    "{:?}",
                    err
                ))),
            }
        }
    }
}
