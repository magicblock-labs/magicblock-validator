use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

pub fn url_encode(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

pub fn is_localhost_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .is_some_and(|url| is_loopback_host(&url))
}

pub fn is_localhost_http_url(url: &str) -> bool {
    is_localhost_url(url)
        && reqwest::Url::parse(url)
            .ok()
            .is_some_and(|url| matches!(url.scheme(), "http" | "https"))
}

pub fn websocket_url_from_rpc_url(url: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(url).ok()?;
    let explicit_port = url.port();
    match url.scheme() {
        "http" => {
            url.set_scheme("ws").ok()?;
            if let Some(port) = explicit_port {
                url.set_port(Some(port.saturating_add(1))).ok()?;
            }
        }
        "https" => {
            url.set_scheme("wss").ok()?;
            if let Some(port) = explicit_port {
                url.set_port(Some(port.saturating_add(1))).ok()?;
            }
        }
        "ws" | "wss" => {}
        _ => return None,
    }
    Some(url.to_string())
}

fn is_loopback_host(url: &reqwest::Url) -> bool {
    url.host_str()
        .map(|host| {
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .to_ascii_lowercase()
        })
        .is_some_and(|host| {
            matches!(
                host.as_str(),
                "localhost" | "127.0.0.1" | "0.0.0.0" | "::1"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{
        is_localhost_http_url, is_localhost_url, websocket_url_from_rpc_url,
    };

    #[test]
    fn localhost_detection_handles_loopback_hosts() {
        assert!(is_localhost_url("http://localhost:8899"));
        assert!(is_localhost_url("http://127.0.0.1:8899"));
        assert!(is_localhost_url("http://0.0.0.0:8899"));
        assert!(is_localhost_url("http://[::1]:8899"));
        assert!(!is_localhost_url("https://api.devnet.solana.com"));
    }

    #[test]
    fn localhost_http_detection_rejects_non_http_schemes() {
        assert!(is_localhost_http_url("http://localhost:8899"));
        assert!(is_localhost_http_url("https://127.0.0.1:8899"));
        assert!(!is_localhost_http_url("ws://127.0.0.1:8900"));
        assert!(!is_localhost_http_url("ftp://localhost"));
    }

    #[test]
    fn websocket_url_is_derived_from_rpc_url() {
        assert_eq!(
            websocket_url_from_rpc_url("http://localhost:8899").as_deref(),
            Some("ws://localhost:8900/")
        );
        assert_eq!(
            websocket_url_from_rpc_url("https://localhost:8443").as_deref(),
            Some("wss://localhost:8444/")
        );
        assert_eq!(
            websocket_url_from_rpc_url("https://localhost").as_deref(),
            Some("wss://localhost/")
        );
        assert_eq!(
            websocket_url_from_rpc_url("ws://localhost:8900").as_deref(),
            Some("ws://localhost:8900/")
        );
        assert_eq!(websocket_url_from_rpc_url("ftp://localhost"), None);
    }
}
