use std::{
    result::Result as TlsResult,
    sync::{Arc, LazyLock},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use fastwebsockets::{FragmentCollectorRead, WebSocket, WebSocketWrite, handshake};
use http_body_util::Empty;
use hyper::{Request, body::Bytes, upgrade::Upgraded};
use hyper_util::rt::{TokioExecutor, TokioIo};
use sha1::{Digest, Sha1};
use tokio::{
    io::{self, AsyncRead, AsyncWrite, ReadHalf, WriteHalf},
    net::TcpStream,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, crypto::ring, pki_types::ServerName},
};
use url::{Host, Position, Url};
use webpki_roots::TLS_SERVER_ROOTS;

use super::{Error, Result};

/// Inbound half that assembles fragmented frames for session validation.
pub(super) type Reader = FragmentCollectorRead<ReadHalf<TokioIo<Upgraded>>>;

/// Outbound half owned exclusively by the socket session.
pub(super) type Writer = WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>;

/// Inclusive limit for an assembled account notification.
pub(super) const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// Shared TLS configuration, initialized only for secure endpoints.
static TLS: LazyLock<TlsResult<TlsConnector, tokio_rustls::rustls::Error>> = LazyLock::new(|| {
    let config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(RootCertStore::from_iter(TLS_SERVER_ROOTS.iter().cloned()))
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
});

/// Opens a provider socket; the session enforces the connection setup timeout.
pub(super) async fn connect(url: &Url) -> Result<(Reader, Writer)> {
    let host = match url.host().ok_or(Error::Protocol("provider URL has no host"))? {
        Host::Ipv6(ip) => ip.to_string(),
        host => host.to_string(),
    };
    let port = url.port_or_known_default().ok_or(Error::Protocol("provider URL has no port"))?;
    let tcp = TcpStream::connect((host.as_str(), port)).await?;
    tcp.set_nodelay(true)?;
    let mut socket = if url.scheme() == "wss" {
        let tls = TLS.as_ref().map_err(|error| Error::Tls(error.clone()))?;
        upgrade(url, tls.connect(ServerName::try_from(host)?, tcp).await?).await?
    } else {
        upgrade(url, tcp).await?
    };
    // Keep all outbound frames on the session's writer, including control replies.
    socket.set_auto_pong(false);
    socket.set_auto_close(false);
    // This bounds individual frames; the session checks assembled messages separately.
    // fastwebsockets rejects lengths >= its limit, whereas MAX_MESSAGE is inclusive.
    socket.set_max_message_size(MAX_MESSAGE + 1);
    let (reader, writer) = socket.split(io::split);
    Ok((FragmentCollectorRead::new(reader), writer))
}

/// Upgrades to WebSocket, checking `Sec-WebSocket-Accept` and rejecting unrequested extensions.
async fn upgrade<S>(url: &Url, stream: S) -> Result<WebSocket<TokioIo<Upgraded>>>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let key = handshake::generate_key();
    let accept = STANDARD.encode(Sha1::digest(format!(
        "{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
    )));
    let request = Request::builder()
        .uri(&url[Position::BeforePath..Position::AfterQuery])
        .header("Host", &url[Position::BeforeHost..Position::AfterPort])
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", key)
        .header("Sec-WebSocket-Version", "13")
        .body(Empty::<Bytes>::new())?;
    let (socket, response) = handshake::client(&TokioExecutor::new(), request, stream).await?;
    // The helper checks upgrade headers but not Sec-WebSocket-Accept or extensions.
    // Verify the response to our key and reject extensions we did not request.
    let headers = response.headers();
    if headers.get("Sec-WebSocket-Accept").map(|value| value.as_bytes()) != Some(accept.as_bytes())
        || headers.contains_key("Sec-WebSocket-Extensions")
        || headers.contains_key("Sec-WebSocket-Protocol")
    {
        return Err(Error::Protocol("invalid WebSocket upgrade response"));
    }
    Ok(socket)
}
