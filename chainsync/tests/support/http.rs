use std::convert::Infallible;

use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use solana_pubkey::Pubkey;
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
};
use url::Url;

use super::{Account, LOOPBACK, Tasks, rpc_result, within};

/// Recorded request held until its scenario releases a response.
pub(crate) struct Call {
    pub(crate) uri: String,
    body: Value,
    reply: oneshot::Sender<(StatusCode, String)>,
}

impl Call {
    /// Ordered `getMultipleAccounts` keys, including companion positions.
    pub(crate) fn keys(&self) -> impl ExactSizeIterator<Item = Pubkey> + '_ {
        self.body["params"][0]
            .as_array()
            .unwrap()
            .iter()
            .map(|key| key.as_str().unwrap().parse().unwrap())
    }

    /// The request's freshness floor, absent when the caller did not specify one.
    pub(crate) fn min_context_slot(&self) -> Option<u64> {
        self.body["params"][1]["minContextSlot"].as_u64()
    }

    /// Releases the held request with its scenario's status and raw response body.
    pub(crate) fn respond(self, status: StatusCode, body: impl Into<String>) {
        self.reply.send((status, body.into())).unwrap();
    }

    /// Preserves null positions and stamps the entire response with one confirmed context slot.
    pub(crate) fn respond_accounts(self, context_slot: u64, accounts: &[Option<Account>]) {
        let values: Vec<_> = accounts.iter().map(|a| a.as_ref().map(Account::rpc)).collect();
        let body = rpc_result(
            &self.body["id"],
            json!({"context":{"slot":context_slot}, "value":values}),
        );
        self.respond(StatusCode::OK, body.to_string());
    }
}

/// Scripted HTTP fixture shared by RPC and AML, with each response explicitly released by its test.
pub(crate) struct Server {
    pub(crate) endpoint: Url,
    calls: mpsc::UnboundedReceiver<Call>,
    tasks: Tasks,
}

impl Server {
    /// Accepts concurrent loopback requests while keeping each response under script control.
    pub(crate) async fn new() -> Self {
        let listener = TcpListener::bind(LOOPBACK).await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap()).parse().unwrap();
        let tasks = Tasks::default();
        let (tx, calls) = mpsc::unbounded_channel();
        let tracker = tasks.clone();
        tasks.spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let tx = tx.clone();
                tracker.spawn(async move {
                    let service = service_fn(move |request| respond(request, tx.clone()));
                    // Clients can disappear during failure scenarios; this connection is finished either way.
                    let _ =
                        http1::Builder::new().serve_connection(TokioIo::new(socket), service).await;
                });
            }
        });
        Self { endpoint, calls, tasks }
    }

    pub(crate) async fn next(&mut self) -> Call {
        within(self.calls.recv()).await.expect("HTTP request")
    }

    /// Consumes one queued request, if any; assertions call this only when no request is expected.
    pub(crate) fn pending(&mut self) -> bool {
        self.calls.try_recv().is_ok()
    }

    /// Cancels held requests and joins the accept loop and every connection task.
    pub(crate) async fn close(self) {
        self.tasks.close().await;
    }
}

/// Records an RPC POST or bodyless AML GET and waits for the scenario's response.
async fn respond(
    request: Request<Incoming>,
    calls: mpsc::UnboundedSender<Call>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let uri = request.uri().to_string();
    let bytes = request.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let (reply, response) = oneshot::channel();
    calls.send(Call { uri, body, reply }).unwrap();
    let (status, body) = response.await.unwrap();
    Ok(Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap())
}
