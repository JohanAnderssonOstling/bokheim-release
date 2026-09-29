#![cfg(not(target_arch = "wasm32"))]

use reqwest::header::HeaderMap;
use reqwest::header::LOCATION;
use reqwest::StatusCode;
use reqwest::{Client, Response, Url};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

const MAX_REDIRECTS: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboundNetworkPolicy {
    /// Device-local clients may intentionally connect to an authority service on their LAN.
    AllowPrivate,
    /// Internet-facing services must never connect to local or reserved networks.
    PublicOnly,
}

#[derive(Debug)]
pub struct OutboundHttpError(String);

impl fmt::Display for OutboundHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for OutboundHttpError {}

impl From<reqwest::Error> for OutboundHttpError {
    fn from(error: reqwest::Error) -> Self {
        Self(error.to_string())
    }
}

fn invalid_target(reason: impl fmt::Display) -> OutboundHttpError {
    OutboundHttpError(format!("outbound HTTP target is not allowed: {reason}"))
}

fn ipv4_is_public(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    !(octets[0] == 0
        || octets[0] == 10
        || octets[0] == 127
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 169 && octets[1] == 254)
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
        || (octets[0] == 192 && octets[1] == 168)
        || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        || octets[0] >= 224)
}

fn ipv6_is_public(address: Ipv6Addr) -> bool {
    if let Some(ipv4) = address.to_ipv4() {
        return ipv4_is_public(ipv4);
    }
    let segments = address.segments();
    // Only globally routed unicast space is accepted. Explicitly exclude
    // documentation and transition ranges that can encode another address.
    (segments[0] & 0xe000) == 0x2000 && !(segments[0] == 0x2001 && segments[1] < 0x0200) && !(segments[0] == 0x2001 && segments[1] == 0x0db8) && segments[0] != 0x2002 && segments[0] != 0x3fff
}

fn ip_is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => ipv4_is_public(address),
        IpAddr::V6(address) => ipv6_is_public(address),
    }
}

fn validate_url(url: &Url) -> Result<(), OutboundHttpError> {
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(invalid_target("URL must be absolute credential-free HTTP(S)"));
    }
    Ok(())
}

fn resolution_host(url: &Url) -> Result<&str, OutboundHttpError> {
    let host = url.host_str().ok_or_else(|| invalid_target("URL has no host"))?;
    Ok(host.strip_prefix('[').and_then(|host| host.strip_suffix(']')).unwrap_or(host))
}

async fn resolved_addresses(url: &Url, policy: OutboundNetworkPolicy) -> Result<Vec<SocketAddr>, OutboundHttpError> {
    validate_url(url)?;
    let host = resolution_host(url)?;
    let port = url.port_or_known_default().ok_or_else(|| invalid_target("URL has no usable port"))?;
    let mut addresses = tokio::net::lookup_host((host, port)).await.map_err(|error| OutboundHttpError(format!("cannot resolve outbound HTTP host: {error}")))?.collect::<Vec<_>>();
    addresses.sort_unstable();
    addresses.dedup();
    if addresses.is_empty() {
        return Err(OutboundHttpError("outbound HTTP host resolved to no addresses".to_owned()));
    }
    if policy == OutboundNetworkPolicy::PublicOnly {
        if let Some(address) = addresses.iter().find(|address| !ip_is_public(address.ip())) {
            return Err(invalid_target(format!("{} resolved to non-public address {}", host, address.ip())));
        }
    }
    Ok(addresses)
}

pub async fn client_for_url(url: &Url, policy: OutboundNetworkPolicy, user_agent: &str, connect_timeout: Duration) -> Result<Client, OutboundHttpError> {
    let addresses = resolved_addresses(url, policy).await?;
    let host = resolution_host(url)?;
    let builder = Client::builder().connect_timeout(connect_timeout).redirect(reqwest::redirect::Policy::none()).no_proxy().user_agent(user_agent);
    let builder = if host.parse::<IpAddr>().is_err() { builder.resolve_to_addrs(host, &addresses) } else { builder };
    builder.build().map_err(Into::into)
}

pub async fn send_get(mut url: Url, headers: HeaderMap, policy: OutboundNetworkPolicy, user_agent: &str, connect_timeout: Duration, response_header_timeout: Duration) -> Result<Response, OutboundHttpError> {
    for redirects in 0..=MAX_REDIRECTS {
        let client = client_for_url(&url, policy, user_agent, connect_timeout).await?;
        let response = tokio::time::timeout(response_header_timeout, client.get(url.clone()).headers(headers.clone()).send()).await.map_err(|_| OutboundHttpError("outbound HTTP server did not respond before the timeout".to_owned()))??;
        if !matches!(response.status(), StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER | StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT) {
            return Ok(response);
        }
        if redirects == MAX_REDIRECTS {
            return Err(OutboundHttpError(format!("outbound HTTP request exceeded {MAX_REDIRECTS} redirects")));
        }
        let location = response.headers().get(LOCATION).ok_or_else(|| OutboundHttpError("outbound HTTP redirect has no Location header".to_owned()))?;
        let location = location.to_str().map_err(|_| OutboundHttpError("outbound HTTP redirect has an invalid Location header".to_owned()))?;
        url = url.join(location).map_err(|error| OutboundHttpError(format!("outbound HTTP redirect URL is invalid: {error}")))?;
        // The next loop validates and pins the redirect target before sending.
    }
    unreachable!("redirect loop returns at its configured bound")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_policy_rejects_internal_and_reserved_ip_ranges() {
        for address in ["0.0.0.0", "10.0.0.1", "100.64.0.1", "127.0.0.1", "169.254.169.254", "172.16.0.1", "192.168.1.1", "198.18.0.1", "224.0.0.1", "::", "::1", "::ffff:127.0.0.1", "fc00::1", "fe80::1", "2001:db8::1"] {
            assert!(!ip_is_public(address.parse().unwrap()), "{address} must not be considered public");
        }
        for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(ip_is_public(address.parse().unwrap()), "{address} should be considered public");
        }
    }

    #[tokio::test]
    async fn public_policy_rejects_loopback_before_connecting() {
        let url = Url::parse("http://127.0.0.1:9/private").unwrap();
        let error = send_get(url, HeaderMap::new(), OutboundNetworkPolicy::PublicOnly, "test", Duration::from_secs(1), Duration::from_secs(1)).await.unwrap_err();
        assert!(error.to_string().contains("non-public address"));
    }

    #[tokio::test]
    async fn redirect_targets_are_subject_to_the_same_public_policy() {
        let public_origin = Url::parse("https://example.com/authority").unwrap();
        for location in ["http://127.0.0.1/admin", "http://[::1]/admin", "http://169.254.169.254/latest/meta-data/", "http://2130706433/admin"] {
            let redirect = public_origin.join(location).unwrap();
            let error = client_for_url(&redirect, OutboundNetworkPolicy::PublicOnly, "test", Duration::from_secs(1)).await.unwrap_err();
            assert!(error.to_string().contains("non-public address"), "redirect target should be rejected: {location}");
        }
    }

    #[tokio::test]
    async fn redirects_are_followed_manually_when_each_hop_is_allowed() {
        use axum::routing::get;
        use axum::Router;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/start", get(|| async { axum::response::Redirect::temporary("/final") })).route("/final", get(|| async { "ok" }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let response = send_get(Url::parse(&format!("http://{address}/start")).unwrap(), HeaderMap::new(), OutboundNetworkPolicy::AllowPrivate, "test", Duration::from_secs(1), Duration::from_secs(1)).await.unwrap();
        assert_eq!(response.url().path(), "/final");
        assert_eq!(response.bytes().await.unwrap(), "ok");
        server.abort();
    }
}
