/// Server availability status codes
use std::error::Error;

/// Server responded successfully to a library probe
pub const ONLINE: i16 = 0;
/// DNS failure, connection refused, reset, etc.
pub const UNREACHABLE: i16 = 1;
/// Connect or request timed out (server too slow / unreachable in time)
pub const TIMEOUT: i16 = 2;
/// TLS handshake or certificate validation failed
pub const TLS: i16 = 3;
/// No remote URI or access token available from plex.tv
pub const NO_CONNECTION: i16 = 4;
/// TCP/TLS succeeded but HTTP response was an error status
pub const HTTP_ERROR: i16 = 5;

pub fn reason_label(status: i16) -> &'static str {
    match status {
        ONLINE => "online",
        UNREACHABLE => "unreachable",
        TIMEOUT => "timeout",
        TLS => "tls",
        NO_CONNECTION => "no_connection",
        HTTP_ERROR => "http_error",
        _ => "unknown",
    }
}

pub fn classify_reqwest_error(err: &reqwest::Error) -> i16 {
    if err.is_timeout() {
        return TIMEOUT;
    }
    if err.status().is_some() {
        return HTTP_ERROR;
    }
    if error_chain_suggests_tls(err) {
        return TLS;
    }
    if err.is_connect() {
        return UNREACHABLE;
    }
    if error_chain_suggests_tls(err) {
        TLS
    } else {
        UNREACHABLE
    }
}

fn error_chain_suggests_tls(err: &reqwest::Error) -> bool {
    let mut current: Option<&(dyn Error + 'static)> = Some(err);
    while let Some(e) = current {
        let msg = e.to_string().to_lowercase();
        if msg.contains("certificate")
            || msg.contains("cert ")
            || msg.contains("tls")
            || msg.contains("ssl")
            || msg.contains("handshake")
            || msg.contains("pkix")
            || msg.contains("unknown issuer")
            || msg.contains("self signed")
            || msg.contains("self-signed")
        {
            return true;
        }
        current = e.source();
    }
    false
}
