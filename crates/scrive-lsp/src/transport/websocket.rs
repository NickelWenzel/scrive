//! The WebSocket bridge: one JSON-RPC message per text frame, no `Content-Length`.

mod native;

pub(crate) use native::{Connection, Endpoint, Pending, Queue, Socket};

use std::time::Duration;

use tungstenite::http::Uri;

use super::{socket, Settings, Started};
use crate::client::builder;

/// Which kind of WebSocket URL was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scheme {
    Plain,
    Secure,
}

/// The scheme of `uri`: `ws` or `wss`, in any case.
pub(crate) fn scheme(uri: &Uri) -> Result<Scheme, builder::error::Url> {
    match uri.scheme_str() {
        Some(scheme) if scheme.eq_ignore_ascii_case("ws") => Ok(Scheme::Plain),
        Some(scheme) if scheme.eq_ignore_ascii_case("wss") => Ok(Scheme::Secure),
        Some(_) | None => Err(builder::error::Url::Scheme),
    }
}

/// Starts a worker that dials `endpoint` and carries one connection at a time. The first dials
/// retry until `budget` has passed.
pub(crate) fn start(
    endpoint: Endpoint,
    budget: Duration,
    settings: Settings,
) -> Result<Started, builder::Error> {
    socket::connect(socket::Endpoint::Websocket(endpoint), budget, settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(url: &str) -> Result<Endpoint, builder::Error> {
        Endpoint::new(url, None)
    }

    fn reason(url: &str) -> Option<builder::error::Url> {
        match parse(url) {
            Err(builder::Error::Url { reason, .. }) => Some(reason),
            Ok(_) | Err(_) => None,
        }
    }

    /// Both schemes parse, whatever their case.
    #[test]
    fn ws_and_wss_parse_in_any_case() {
        for (url, expected) in [
            ("ws://127.0.0.1:3000/x", Scheme::Plain),
            ("WS://localhost", Scheme::Plain),
            ("wss://example.com", Scheme::Secure),
            ("WSS://example.com", Scheme::Secure),
        ] {
            let uri: Uri = url.parse().expect("the URL parses");
            assert_eq!(scheme(&uri), Ok(expected), "{url}");
            assert!(parse(url).is_ok(), "{url} is an endpoint");
        }
    }

    /// A URL with no host, or that doesn't parse, can't be dialled.
    #[test]
    fn a_url_without_a_host_is_unparsable() {
        for url in ["ws:/path", "ws://exa mple", "not a url"] {
            assert_eq!(reason(url), Some(builder::error::Url::Unparsable), "{url}");
        }
    }

    /// Anything but `ws` and `wss` is a scheme error.
    #[test]
    fn an_http_url_is_a_scheme_error() {
        for url in ["http://localhost", "https://localhost:8443/"] {
            assert_eq!(reason(url), Some(builder::error::Url::Scheme), "{url}");
        }
    }
}
