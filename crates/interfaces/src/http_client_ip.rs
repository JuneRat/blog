//! Shared, fail-closed source address extraction for comments and business audits.
use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{HeaderMap, request::Parts},
};
use std::{
    convert::Infallible,
    net::{IpAddr, SocketAddr},
};

#[derive(Clone, Default)]
pub struct TrustedProxies(pub Vec<IpAddr>);

#[derive(Clone, Copy, Default)]
pub struct ClientAddress(pub Option<IpAddr>);

/// Rate limits must retain a source bucket even when a trusted proxy supplies
/// invalid/missing forwarding data. Audits still leave that client IP unknown.
pub struct ClientRateLimitKey(pub Option<String>);
impl<S: Send + Sync> FromRequestParts<S> for ClientRateLimitKey {
    type Rejection = Infallible;
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            ClientAddress::from_parts(parts)
                .0
                .or_else(|| {
                    parts
                        .extensions
                        .get::<ConnectInfo<SocketAddr>>()
                        .map(|p| p.0.ip())
                })
                .map(|ip| ip.to_string()),
        ))
    }
}
impl ClientAddress {
    pub fn from_parts(parts: &Parts) -> Self {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|p| p.0.ip());
        let trusted = parts
            .extensions
            .get::<TrustedProxies>()
            .map(|p| p.0.as_slice())
            .unwrap_or(&[]);
        Self(client_ip(peer, &parts.headers, trusted))
    }
}
impl<S: Send + Sync> FromRequestParts<S> for ClientAddress {
    type Rejection = Infallible;
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::from_parts(parts))
    }
}

// Only configured socket peers can supply X-Forwarded-For. Walk from the
// nearest hop; a client-controlled prefix cannot override the first untrusted IP.
pub(crate) fn client_ip(
    peer: Option<IpAddr>,
    headers: &HeaderMap,
    trusted: &[IpAddr],
) -> Option<IpAddr> {
    let peer = peer?;
    if !trusted.contains(&peer) {
        return Some(peer);
    }
    let mut addresses = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        for raw in value.to_str().ok()?.split(',') {
            if addresses.len() >= 20 {
                return None;
            }
            addresses.push(raw.trim().parse::<IpAddr>().ok()?);
        }
    }
    addresses.into_iter().rev().find(|ip| !trusted.contains(ip))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_or_invalid_forwarding_never_invents_a_client() {
        let proxy = "127.0.0.1".parse().unwrap();
        let client = "2001:db8::1".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "203.0.113.9, 2001:db8::1, 127.0.0.1".parse().unwrap(),
        );
        assert_eq!(client_ip(Some(proxy), &headers, &[proxy]), Some(client));
        assert_eq!(client_ip(Some(client), &headers, &[proxy]), Some(client));
        assert_eq!(client_ip(Some(proxy), &headers, &[]), Some(proxy));
        assert_eq!(client_ip(None, &headers, &[proxy]), None);
        for raw in ["bad", "127.0.0.1", "", "unknown, 2001:db8::1"] {
            headers.insert("x-forwarded-for", raw.parse().unwrap());
            assert_eq!(client_ip(Some(proxy), &headers, &[proxy]), None);
        }
        headers.clear();
        assert_eq!(client_ip(Some(proxy), &headers, &[proxy]), None);
        headers.insert(
            "x-forwarded-for",
            vec!["198.51.100.1"; 21].join(",").parse().unwrap(),
        );
        assert_eq!(client_ip(Some(proxy), &headers, &[proxy]), None);
    }
}
