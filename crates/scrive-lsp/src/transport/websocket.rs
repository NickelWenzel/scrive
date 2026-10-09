//! The WebSocket bridge: one JSON-RPC message per text frame, no `Content-Length`.

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod browser;
#[cfg(not(target_family = "wasm"))]
mod native;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(crate) use browser::{start, Control, Endpoint};
#[cfg(not(target_family = "wasm"))]
pub(crate) use native::{Connection, Endpoint, Pending, Queue, Socket};

#[cfg(not(target_family = "wasm"))]
use std::time::Duration;

#[cfg(not(target_family = "wasm"))]
use tungstenite::http::Uri;

#[cfg(not(target_family = "wasm"))]
use super::{socket, Settings, Started};
use crate::client::builder;

/// Which kind of WebSocket URL was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scheme {
    Plain,
    Secure,
}

/// The scheme of `uri`: `ws` or `wss`, in any case.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn scheme(uri: &Uri) -> Result<Scheme, builder::error::Url> {
    match uri.scheme_str() {
        Some(scheme) if scheme.eq_ignore_ascii_case("ws") => Ok(Scheme::Plain),
        Some(scheme) if scheme.eq_ignore_ascii_case("wss") => Ok(Scheme::Secure),
        Some(_) | None => Err(builder::error::Url::Scheme),
    }
}

/// The scheme of `url`, `ws` or `wss` in any case, read up to its `://`. The browser parses the
/// rest when it opens the socket.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(crate) fn scheme(url: &str) -> Result<Scheme, builder::error::Url> {
    let Some((scheme, _)) = url.split_once("://") else {
        return Err(builder::error::Url::Unparsable);
    };
    if scheme.eq_ignore_ascii_case("ws") {
        Ok(Scheme::Plain)
    } else if scheme.eq_ignore_ascii_case("wss") {
        Ok(Scheme::Secure)
    } else {
        Err(builder::error::Url::Scheme)
    }
}

/// Starts a worker that dials `endpoint` and carries one connection at a time. The first dials
/// retry until `budget` has passed.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn start(
    endpoint: Endpoint,
    budget: Duration,
    settings: Settings,
) -> Result<Started, builder::Error> {
    socket::connect(socket::Endpoint::Websocket(endpoint), budget, settings)
}

#[cfg(all(test, not(target_family = "wasm")))]
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
