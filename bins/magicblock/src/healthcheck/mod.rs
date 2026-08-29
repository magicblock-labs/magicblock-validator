mod deadline;
mod expression;

use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use futures_util::Stream;
use humantime::re::humantime as ht;
use magicblock_config::{consts::HEALTHCHECK_ACCOUNT_PUBKEY, types::Remote};
use nucleus::metrics::EventTimer;
use serde::Deserialize;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::response::{Response, RpcSignatureResult};
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tracing::info;

use self::{deadline::Deadline, expression::random};

#[derive(clap::Args, Deserialize)]
pub struct Args {
    /// Validator HTTP RPC URL.
    #[arg(long)]
    url: Remote,
    /// End-to-end deadline, such as 5s or 1m.
    #[arg(long, value_parser = ht::parse_duration)]
    timeout: Duration,
}

impl Args {
    pub async fn run(self) -> Result<()> {
        let mut timer = EventTimer::new("healthcheck");
        let deadline = Deadline::new(self.timeout);
        let rpc_url = self.url.url_str().to_owned();
        let ws_url = self
            .url
            .to_websocket()
            .context("healthcheck requires an HTTP RPC URL")?
            .url_str()
            .to_owned();
        let rpc = RpcClient::new_with_commitment(
            rpc_url.clone(),
            CommitmentConfig::processed(),
        );
        info!(rpc = %rpc_url, websocket = %ws_url, timeout = ?self.timeout, "Starting healthcheck");

        let signer = Keypair::new();
        let blockhash = deadline
            .run("fetching latest blockhash", rpc.get_latest_blockhash())
            .await
            .with_context(|| format!("RPC {rpc_url}"))?;
        let transaction = Transaction::new_signed_with_payer(
            &[random().compose(HEALTHCHECK_ACCOUNT_PUBKEY, &[])],
            Some(&signer.pubkey()),
            &[&signer],
            blockhash,
        );
        let signature = transaction.signatures[0];
        timer.record("transaction built");
        info!(%signature, "Built randomized v42 transaction");

        let pubsub = deadline
            .run("connecting to WebSocket RPC", PubsubClient::new(&ws_url))
            .await
            .with_context(|| format!("WebSocket {ws_url}"))?;
        let (signatures, _) = deadline
            .run(
                "registering signature subscription",
                pubsub.signature_subscribe(&signature, None),
            )
            .await
            .with_context(|| format!("signatureSubscribe {signature}"))?;
        let (accounts, _) = deadline
            .run(
                "registering account subscription",
                pubsub.account_subscribe(&HEALTHCHECK_ACCOUNT_PUBKEY, None),
            )
            .await
            .with_context(|| {
                format!("accountSubscribe {HEALTHCHECK_ACCOUNT_PUBKEY}")
            })?;
        timer.record("subscriptions registered");

        let returned = deadline
            .run("sending transaction", rpc.send_transaction(&transaction))
            .await
            .with_context(|| format!("sendTransaction via {rpc_url}"))?;
        ensure!(
            returned == signature,
            "sendTransaction returned {returned}, expected {signature}"
        );
        timer.record("transaction submitted");
        info!(%signature, "Submitted healthcheck transaction");

        info!(%signature, "Waiting for execution and account update");
        tokio::try_join!(
            verify_signature(deadline, &rpc, signature, signatures),
            wait_for_update(deadline, accounts),
        )?;
        timer.record("execution and account update verified");

        println!("healthcheck succeeded: {signature}");
        Ok(())
    }
}

async fn verify_signature(
    deadline: Deadline,
    rpc: &RpcClient,
    signature: Signature,
    mut stream: impl Stream<Item = Response<RpcSignatureResult>> + Unpin,
) -> Result<()> {
    deadline
        .next("waiting for signature notification", &mut stream)
        .await?;
    info!(%signature,  "Received successful signature notification");

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
    status.status.map_err(|error| {
        anyhow!("getSignatureStatuses reported execution failure: {error:?}")
    })?;
    info!(%signature, "Verified signature status");
    Ok(())
}

async fn wait_for_update<T>(
    deadline: Deadline,
    mut stream: impl Stream<Item = T> + Unpin,
) -> Result<()> {
    deadline
        .next("waiting for account notification", &mut stream)
        .await?;
    info!("Received account notification");
    Ok(())
}
