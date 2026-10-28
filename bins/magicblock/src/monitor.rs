//! Passive, best-effort observation of Engine execution failures through Aperture.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use config::types::Remote;
use futures_util::{SinkExt, StreamExt};
use json::Value;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

/// Dedicated connection whose lifetime also owns the server-side subscription.
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Endpoint for passive monitoring; no signing identity or probe transaction is needed.
#[derive(clap::Args)]
pub(crate) struct Args {
    /// Validator HTTP RPC or WebSocket URL.
    #[arg(long)]
    url: Remote,
}

impl Args {
    /// Bounds connection setup, then logs live failures until Ctrl-C or disconnection.
    /// Idle monitoring has no deadline and does not detect missed notifications.
    pub(crate) async fn run(self) -> Result<()> {
        let url = self.url.to_websocket().context("monitor requires an HTTP or WebSocket URL")?;
        let mut ws = timeout(Duration::from_secs(10), subscribe(url.url_str()))
            .await
            .context("timed out connecting and subscribing")??;
        info!(websocket = %url, "Monitoring transaction failures");
        let interrupt = tokio::signal::ctrl_c();
        tokio::pin!(interrupt);
        loop {
            let notification = tokio::select! {
                result = &mut interrupt => {
                    result.context("listening for Ctrl-C")?;
                    // Aperture owns subscriptions per connection; no separate unsubscribe is needed.
                    let _ = timeout(Duration::from_secs(1), ws.close(None)).await;
                    return Ok(());
                },
                notification = read_json(&mut ws) => notification?,
            };
            let event = &notification["params"]["result"];
            warn!(
                signature = event["signature"].as_str(),
                slot = event["slot"].as_u64(),
                error = %event["error"],
                "Transaction failed"
            );
            debug!(failure = %event, "Transaction failure details");
        }
    }
}

/// Consumes the subscription acknowledgement before handing the socket to the monitor.
/// This connection carries only one subscription, so updates need no local routing.
async fn subscribe(url: &str) -> Result<Socket> {
    let (mut ws, _) = connect_async(url).await.context("connecting to WebSocket RPC")?;
    ws.send(Message::text(json::to_string(&json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "transactionFailureSubscribe", "params": []
    }))?))
    .await?;
    let response = read_json(&mut ws).await.context("receiving subscription response")?;
    if !response["error"].is_null() {
        bail!("failure subscription rejected: {}", response["error"]);
    }
    Ok(ws)
}

/// Reads text messages, skipping control frames and treating transport closure as an error.
/// Failure payloads come from the paired Aperture server and are not schema-validated here.
async fn read_json(ws: &mut Socket) -> Result<Value> {
    while let Some(message) = ws.next().await {
        match message? {
            Message::Text(text) => {
                return json::from_str(&text).context("malformed WebSocket JSON");
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    bail!("failure stream disconnected")
}
