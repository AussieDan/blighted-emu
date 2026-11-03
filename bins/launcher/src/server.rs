// SPDX-License-Identifier: GPL-3.0-or-later
//! The server address a player types, turned into the launcher API base URL.

use nw_protocol::launcher_api::is_forbidden_host;

/// Host label of the launcher API: `login.<domain>`.
pub const LOGIN_HOST_LABEL: &str = "login";
/// The HTTPS port of a base URL that names none.
pub const DEFAULT_HTTPS_PORT: u16 = 443;
const HTTPS_PREFIX: &str = "https://";
/// Separator between the host and the port.
const PORT_SEPARATOR: char = ':';

/// A server address that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServerAddressError {
    /// Empty, or not a plain domain name with an optional port.
    #[error("enter the server domain, for example example.org or example.org:8443")]
    Invalid,
    /// An Amazon or CloudFront domain (master spec 4.1).
    #[error("Amazon and CloudFront addresses are refused")]
    Forbidden,
}

/// The host and port of a base URL from [`api_base_url`]: `https://login.example.org:8443` gives
/// `login.example.org:8443`.
pub fn api_host(base_url: &str) -> &str {
    base_url.trim_start_matches(HTTPS_PREFIX)
}

/// The host name of a base URL from [`api_base_url`], without the port: the name to look up.
pub fn api_host_name(base_url: &str) -> &str {
    let authority = api_host(base_url);
    authority
        .split_once(PORT_SEPARATOR)
        .map_or(authority, |(host, _)| host)
}

/// The HTTPS port of a base URL from [`api_base_url`]: the named port, else
/// [`DEFAULT_HTTPS_PORT`].
pub fn api_port(base_url: &str) -> u16 {
    api_host(base_url)
        .split_once(PORT_SEPARATOR)
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or(DEFAULT_HTTPS_PORT)
}

/// The port text after a host: digits of a port above 0. [`DEFAULT_HTTPS_PORT`] gives `None`, so
/// a base URL never names it.
fn typed_port(text: &str) -> Result<Option<u16>, ServerAddressError> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ServerAddressError::Invalid);
    }
    match text.parse::<u16>() {
        Ok(0) | Err(_) => Err(ServerAddressError::Invalid),
        Ok(DEFAULT_HTTPS_PORT) => Ok(None),
        Ok(port) => Ok(Some(port)),
    }
}

/// The launcher API base URL for a typed address: `example.org`, `login.example.org` and
/// `https://login.example.org` all give `https://login.example.org`. A port after the domain
/// stays on the URL (`example.org:8443` gives `https://login.example.org:8443`), except 443.
pub fn api_base_url(input: &str) -> Result<String, ServerAddressError> {
    let typed = input
        .trim()
        .trim_start_matches(HTTPS_PREFIX)
        .trim_end_matches('/')
        .to_ascii_lowercase();
    let (host, port) = match typed.split_once(PORT_SEPARATOR) {
        Some((host, port)) => (host.to_owned(), typed_port(port)?),
        None => (typed, None),
    };
    let valid = !host.is_empty()
        && host.contains('.')
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-');
    if !valid {
        return Err(ServerAddressError::Invalid);
    }
    if is_forbidden_host(&host) {
        return Err(ServerAddressError::Forbidden);
    }
    let login_prefix = format!("{LOGIN_HOST_LABEL}.");
    let login_host = if host.starts_with(&login_prefix) {
        host
    } else {
        format!("{login_prefix}{host}")
    };
    Ok(match port {
        Some(port) => format!("{HTTPS_PREFIX}{login_host}{PORT_SEPARATOR}{port}"),
        None => format!("{HTTPS_PREFIX}{login_host}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_forms_give_the_login_host() {
        for input in [
            "example.org",
            "login.example.org",
            "https://login.example.org/",
            " Example.ORG ",
        ] {
            assert_eq!(
                api_base_url(input).as_deref(),
                Ok("https://login.example.org"),
                "{input}"
            );
        }
    }

    #[test]
    fn the_host_is_the_base_url_without_the_scheme() {
        let base = api_base_url("example.org").expect("base");
        assert_eq!(api_host(&base), "login.example.org");
        assert_eq!(api_host_name(&base), "login.example.org");
        assert_eq!(api_port(&base), DEFAULT_HTTPS_PORT);
    }

    #[test]
    fn a_typed_port_stays_on_the_base_url() {
        const TYPED_PORT: u16 = 8443;
        for input in [
            "example.org:8443",
            "login.example.org:8443",
            "https://login.example.org:8443/",
        ] {
            let base = api_base_url(input).expect("base");
            assert_eq!(base, "https://login.example.org:8443", "{input}");
            assert_eq!(api_host(&base), "login.example.org:8443");
            assert_eq!(api_host_name(&base), "login.example.org");
            assert_eq!(api_port(&base), TYPED_PORT);
        }
        assert_eq!(
            api_base_url("example.org:443").as_deref(),
            Ok("https://login.example.org")
        );
    }

    #[test]
    fn bad_and_forbidden_addresses_are_refused() {
        assert_eq!(api_base_url(""), Err(ServerAddressError::Invalid));
        assert_eq!(api_base_url("localhost"), Err(ServerAddressError::Invalid));
        assert_eq!(
            api_base_url("http://example.org"),
            Err(ServerAddressError::Invalid)
        );
        assert_eq!(
            api_base_url("example.org/path"),
            Err(ServerAddressError::Invalid)
        );
        for bad_port in [
            "example.org:",
            "example.org:0",
            "example.org:65536",
            "example.org:+80",
            "example.org:80:81",
        ] {
            assert_eq!(
                api_base_url(bad_port),
                Err(ServerAddressError::Invalid),
                "{bad_port}"
            );
        }
        assert_eq!(
            api_base_url("d2c74t4zimux3r.cloudfront.net:8443"),
            Err(ServerAddressError::Forbidden)
        );
        assert_eq!(
            api_base_url("d2c74t4zimux3r.cloudfront.net"),
            Err(ServerAddressError::Forbidden)
        );
        assert_eq!(
            api_base_url("login.amazongames.com"),
            Err(ServerAddressError::Forbidden)
        );
    }
}
