use std::net::SocketAddr;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use serde_json::Value;

use crate::error::{normalize_network_error, Result, YsptpError};
use crate::util;

#[derive(Clone, Debug)]
struct Ipv4OnlyResolver;

const FALLBACK_DNS: [&str; 3] = ["223.5.5.5:53", "119.29.29.29:53", "114.114.114.114:53"];

fn build_dns_query(host: &str, id: u16) -> Option<Vec<u8>> {
    let mut q = Vec::with_capacity(32 + host.len());
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
    Some(q)
}

fn skip_dns_name(buf: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let len = *buf.get(i)? as usize;
        if len == 0 {
            return Some(i + 1);
        }
        if len & 0xC0 == 0xC0 {
            return Some(i + 2);
        }
        i += len + 1;
    }
}

fn parse_dns_a(buf: &[u8], id: u16) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    if buf.len() < 12 || u16::from_be_bytes([buf[0], buf[1]]) != id || buf[3] & 0x0F != 0 {
        return out;
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut i = 12;
    for _ in 0..qd {
        match skip_dns_name(buf, i) {
            Some(n) => i = n + 4,
            None => return out,
        }
    }
    for _ in 0..an {
        let Some(n) = skip_dns_name(buf, i) else { return out };
        if n + 10 > buf.len() {
            return out;
        }
        let typ = u16::from_be_bytes([buf[n], buf[n + 1]]);
        let rdlen = u16::from_be_bytes([buf[n + 8], buf[n + 9]]) as usize;
        let data = n + 10;
        if data + rdlen > buf.len() {
            return out;
        }
        if typ == 1 && rdlen == 4 {
            let ip = std::net::Ipv4Addr::new(buf[data], buf[data + 1], buf[data + 2], buf[data + 3]);
            out.push(SocketAddr::new(ip.into(), 0));
        }
        i = data + rdlen;
    }
    out
}

async fn fallback_lookup(host: &str) -> Vec<SocketAddr> {
    let id = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(1)
        & 0xFFFF) as u16;
    let Some(query) = build_dns_query(host, id) else { return Vec::new() };
    for server in FALLBACK_DNS {
        let Ok(sock) = tokio::net::UdpSocket::bind("0.0.0.0:0").await else { continue };
        if sock.connect(server).await.is_err() || sock.send(&query).await.is_err() {
            continue;
        }
        let mut buf = [0u8; 1500];
        if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(3), sock.recv(&mut buf)).await {
            let addrs = parse_dns_a(&buf[..n], id);
            if !addrs.is_empty() {
                return addrs;
            }
        }
    }
    Vec::new()
}

impl reqwest::dns::Resolve for Ipv4OnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let sys: std::result::Result<Vec<SocketAddr>, std::io::Error> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map(|it| it.filter(|a| a.is_ipv4()).collect());
            let addrs = match sys {
                Ok(v) if !v.is_empty() => v,
                Ok(_) => fallback_lookup(&host).await,
                Err(e) => {
                    let v = fallback_lookup(&host).await;
                    if v.is_empty() {
                        return Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>);
                    }
                    v
                }
            };
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub reason: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub final_url: String,
}

impl HttpResponse {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,

    no_redirect: reqwest::Client,
    timeout: Duration,
}

fn is_dns_jitter_error(e: &reqwest::Error) -> bool {
    let s = e.to_string().to_lowercase();
    ["-3", "getaddrinfo", "temporary failure", "timed out", "connection reset", "dns"]
        .iter()
        .any(|k| s.contains(k))
}

fn to_header_map(headers: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (k, v) in headers {
        if let (Ok(name), Ok(value)) = (
            k.parse::<HeaderName>(),
            HeaderValue::from_str(&util::header_value_clean(v)),
        ) {
            map.insert(name, value);
        }
    }
    map
}

impl HttpClient {
    pub fn new(timeout: f64, insecure_tls: bool) -> Result<Self> {
        let timeout = Duration::from_secs_f64(timeout.max(0.1));
        let builder = reqwest::Client::builder()

            .no_proxy()

            .http1_only()

            .cookie_store(true)

            .redirect(Policy::limited(10))
            .timeout(timeout)
            .connect_timeout(timeout)
            .danger_accept_invalid_certs(insecure_tls)
            .dns_resolver(std::sync::Arc::new(Ipv4OnlyResolver));
        let inner = builder
            .build()
            .map_err(|e| YsptpError::new(format!("http client build failed: {e}")))?;
        let no_redirect = reqwest::Client::builder()
            .no_proxy()
            .http1_only()
            .cookie_store(true)
            .redirect(Policy::none())
            .timeout(timeout)
            .connect_timeout(timeout)
            .danger_accept_invalid_certs(insecure_tls)
            .dns_resolver(std::sync::Arc::new(Ipv4OnlyResolver))
            .build()
            .map_err(|e| YsptpError::new(format!("http raw client build failed: {e}")))?;
        Ok(Self { inner, no_redirect, timeout })
    }

    pub fn raw_client(&self) -> reqwest::Client {
        self.no_redirect.clone()
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        headers: &[(&str, &str)],
        data: Option<Vec<u8>>,
    ) -> Result<HttpResponse> {
        self.send_timeout(method, url, headers, data, self.timeout).await
    }

    pub async fn send_timeout(
        &self,
        method: reqwest::Method,
        url: &str,
        headers: &[(&str, &str)],
        data: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<HttpResponse> {
        let hm = to_header_map(headers);
        let mut last_err: Option<YsptpError> = None;
        for attempt in 0..=2u32 {
            let mut req = self.inner.request(method.clone(), url).headers(hm.clone()).timeout(timeout);
            if let Some(ref body) = data {
                req = req.body(body.clone());
            }
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let reason = resp
                        .status()
                        .canonical_reason()
                        .unwrap_or("")
                        .to_string();
                    let final_url = resp.url().to_string();
                    let headers = resp.headers().clone();
                    let body = resp.bytes().await.map_err(|e| YsptpError::new(normalize_network_error(&e)))?;
                    let mut body = body.to_vec();

                    if headers
                        .get("content-encoding")
                        .and_then(|v| v.to_str().ok())
                        .map(|v| v.to_lowercase().contains("gzip"))
                        .unwrap_or(false)
                        && body.starts_with(&[0x1f, 0x8b])
                    {
                        use std::io::Read as _;
                        let mut decoded = Vec::new();
                        if flate2::read::GzDecoder::new(&body[..]).read_to_end(&mut decoded).is_ok() {
                            body = decoded;
                        }
                    }
                    return Ok(HttpResponse { status, reason, headers, body, final_url });
                }
                Err(e) => {
                    if is_dns_jitter_error(&e) && attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
                        last_err = Some(YsptpError::new(normalize_network_error(&e)));
                        continue;
                    }
                    return Err(YsptpError::new(normalize_network_error(&e)));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| YsptpError::new("network error")))
    }

    pub async fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        self.send(reqwest::Method::GET, url, headers, None).await
    }

    pub async fn post_bytes(&self, url: &str, headers: &[(&str, &str)], data: Vec<u8>) -> Result<HttpResponse> {
        self.send(reqwest::Method::POST, url, headers, Some(data)).await
    }

    pub async fn post_json(&self, url: &str, headers: &[(&str, &str)], body: &Value) -> Result<HttpResponse> {
        let data = util::compact_json_bytes(body, false);
        self.send(reqwest::Method::POST, url, headers, Some(data)).await
    }

    pub async fn post_form(&self, url: &str, headers: &[(&str, &str)], pairs: &[(&str, &str)]) -> Result<HttpResponse> {
        let data = util::form_encode(pairs);
        self.send(reqwest::Method::POST, url, headers, Some(data)).await
    }
}

pub fn response_json(resp: &HttpResponse) -> (u16, String, Option<Value>) {
    let text = resp.text();
    let value = serde_json::from_str(&text).ok();
    (resp.status, text, value)
}

pub async fn raw_get(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
    timeout: Duration,
) -> std::result::Result<reqwest::Response, reqwest::Error> {
    let mut b = client.get(url).headers(to_header_map(headers));
    b = b.timeout(timeout);
    b.send().await
}

#[cfg(test)]
mod dns_tests {
    use super::*;

    #[test]
    fn query_and_parse() {
        let q = build_dns_query("ytpaddr.cctv.cn", 0x1234).unwrap();
        assert_eq!(&q[..2], &[0x12, 0x34]);
        let mut resp = q.clone();
        resp[2] = 0x81;
        resp[3] = 0x80;
        resp[7] = 1;
        resp.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 203, 107, 52, 126]);
        let a = parse_dns_a(&resp, 0x1234);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].ip().to_string(), "203.107.52.126");
        assert!(parse_dns_a(&resp, 0x9999).is_empty());
    }
}
