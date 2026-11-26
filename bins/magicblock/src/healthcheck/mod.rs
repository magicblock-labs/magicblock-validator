//! One-shot probe of transaction submission, execution status, and subscription delivery.

mod deadline;
mod expression;
mod progress;

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use config::{consts::HEALTHCHECK_ACCOUNT_PUBKEY, types::Remote};
use futures_util::Stream;
use humantime::re::humantime as ht;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::response::{Response, RpcSignatureResult};
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tracing::{debug, info};
use url::Url;

use self::{deadline::Deadline, expression::random};

/// Target validator and the total budget for its end-to-end execution probe.
#[derive(clap::Args)]
pub(crate) struct Args {
    /// Validator HTTP RPC URL or cluster alias (mainnet, testnet, devnet, localhost, dev).
    #[arg(long)]
    url: Remote,
    /// End-to-end deadline, such as 5s or 1m.
    #[arg(long, value_parser = ht::parse_duration)]
    timeout: Duration,
    #[command(flatten)]
    progress: Option<progress::SlotProgress>,
}

impl Args {
    /// Subscribes before submitting the probe, then requires both successful signature
    /// status and an account notification under the same end-to-end deadline.
    pub(crate) async fn run(self) -> Result<()> {
        let deadline = Deadline::new(self.timeout);
        let Some(Remote::Websocket(ws_url, _)) = self.url.to_websocket() else {
            return Err(anyhow!("healthcheck requires an HTTP RPC URL"));
        };
        info!(rpc = %self.url, websocket = %ws_url, timeout = ?self.timeout, "Starting healthcheck");
        let rpc = RpcClient::new_with_commitment(
            self.url.url_str().to_owned(),
            CommitmentConfig::processed(),
        );
        let (transaction, slots) = tokio::join!(Self::probe(deadline, &rpc, &ws_url), async {
            match self.progress {
                Some(progress) => progress.check(deadline, &rpc).await,
                None => Ok(()),
            }
        });
        let signature = match (transaction, slots) {
            (Err(transaction), Err(slots)) => {
                return Err(
                    transaction.context(format!("Slot progress check also failed: {slots:#}"))
                );
            }
            (transaction, slots) => slots.and(transaction)?,
        };
        info!(%signature, "Healthcheck succeeded");
        Ok(())
    }

    async fn probe(deadline: Deadline, rpc: &RpcClient, ws_url: &Url) -> Result<Signature> {
        let signer = Keypair::new();
        let blockhash = deadline
            .run("fetching latest blockhash", rpc.get_latest_blockhash())
            .await
            .with_context(|| format!("RPC {}", rpc.url()))?;
        let transaction = Transaction::new_signed_with_payer(
            &[random().compose(HEALTHCHECK_ACCOUNT_PUBKEY, &[])],
            Some(&signer.pubkey()),
            &[&signer],
            blockhash,
        );
        let signature = transaction.signatures[0];

        // Execution can complete before sendTransaction returns, so both streams must be ready first.
        let pubsub = deadline
            .run(
                "connecting to WebSocket RPC",
                PubsubClient::new(ws_url.as_str()),
            )
            .await
            .with_context(|| format!("WebSocket {ws_url}"))?;
        let (signatures, _) = deadline
            .run(
                "registering signature subscription",
                pubsub.signature_subscribe(&signature, None),
            )
            .await
            .with_context(|| format!("signatureSubscribe {signature}"))?;
        let (mut accounts, _) = deadline
            .run(
                "registering account subscription",
                pubsub.account_subscribe(&HEALTHCHECK_ACCOUNT_PUBKEY, None),
            )
            .await
            .with_context(|| format!("accountSubscribe {HEALTHCHECK_ACCOUNT_PUBKEY}"))?;

        deadline
            .run("sending transaction", rpc.send_transaction(&transaction))
            .await
            .with_context(|| format!("sendTransaction via {}", rpc.url()))?;
        debug!(%signature, "Submitted healthcheck transaction");

        // The shared account stream checks delivery; signature status identifies our own probe.
        tokio::try_join!(
            verify_signature(deadline, rpc, signature, signatures),
            deadline.next("waiting for account notification", &mut accounts),
        )?;

        Ok(signature)
    }
}

/// Uses the notification as a completion signal, then checks the RPC's execution result.
/// Notification arrival alone is not treated as proof of successful execution.
async fn verify_signature(
    deadline: Deadline,
    rpc: &RpcClient,
    signature: Signature,
    mut stream: impl Stream<Item = Response<RpcSignatureResult>> + Unpin,
) -> Result<()> {
    deadline.next("waiting for signature notification", &mut stream).await?;

    let statuses = deadline
        .run(
            "checking signature status",
            rpc.get_signature_statuses(&[signature]),
        )
        .await
        .context("getSignatureStatuses failed")?;
    let status = statuses
        .value
        .into_iter()
        .next()
        .flatten()
        .context("getSignatureStatuses returned no transaction status")?;
    status
        .status
        .map_err(|error| anyhow!("getSignatureStatuses reported execution failure: {error:?}"))
}
