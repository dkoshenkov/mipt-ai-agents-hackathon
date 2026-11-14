//! Direct source reads are independently bounded and never turn a search summary into proof.
use super::{materials, types::*};
use std::{
    collections::BTreeMap,
    future::Future,
    net::{IpAddr, SocketAddr},
    process::Stdio,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::{Host, Url};

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: String,
    pub location: Option<String>,
    pub body: Vec<u8>,
    pub retrieved_at: String,
}

pub trait HttpTransport: Send + Sync {
    /// Validate/pin DNS before a request is charged. Implementations must not send HTTP here.
    fn prepare(
        &self,
        _url: &Url,
    ) -> impl Future<Output = Result<Vec<SocketAddr>, FailureReason>> + Send {
        async { Ok(vec![]) }
    }
    /// One physical request. No automatic redirects or retries; never log the URL/body.
    fn get(
        &self,
        url: &Url,
        addresses: &[SocketAddr],
        max_bytes: usize,
    ) -> impl Future<Output = Result<HttpResponse, FailureReason>> + Send;
}

pub struct PublicHttp;
impl HttpTransport for PublicHttp {
    async fn prepare(&self, url: &Url) -> Result<Vec<SocketAddr>, FailureReason> {
        let host = url.host_str().ok_or(FailureReason::AddressRejected)?;
        let addresses: Vec<_> = tokio::net::lookup_host((host.trim_matches(['[', ']']), 443))
            .await
            .map_err(|_| FailureReason::SourceUnavailable)?
            .collect();
        if addresses.is_empty() || addresses.iter().any(|a| !public_ip(a.ip())) {
            return Err(FailureReason::AddressRejected);
        }
        Ok(addresses)
    }
    async fn get(
        &self,
        url: &Url,
        addresses: &[SocketAddr],
        max_bytes: usize,
    ) -> Result<HttpResponse, FailureReason> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .resolve_to_addrs(
                url.host_str().ok_or(FailureReason::AddressRejected)?,
                addresses,
            )
            .build()
            .map_err(|_| FailureReason::SourceUnavailable)?;
        let mut response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|_| FailureReason::SourceUnavailable)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if response
            .content_length()
            .is_some_and(|n| n > max_bytes as u64)
        {
            return Err(FailureReason::TooLarge);
        }
        let mut body = vec![];
        // Redirect bodies are irrelevant. Count the redirect, validate its destination first.
        if !(300..400).contains(&status) {
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| FailureReason::SourceUnavailable)?
            {
                if body.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(FailureReason::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
        }
        Ok(HttpResponse {
            status,
            content_type,
            location,
            body,
            retrieved_at: format!(
                "unix:{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            ),
        })
    }
}

#[derive(Default)]
pub struct SnapshotHttp {
    pub responses: BTreeMap<String, Result<HttpResponse, FailureReason>>,
    calls: Mutex<Vec<String>>,
}
impl SnapshotHttp {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    pub fn insert(&mut self, url: &str, content_type: &str, bytes: Vec<u8>) {
        self.responses.insert(
            url.into(),
            Ok(HttpResponse {
                status: 200,
                content_type: content_type.into(),
                location: None,
                body: bytes,
                retrieved_at: "snapshot".into(),
            }),
        );
    }
}
impl HttpTransport for SnapshotHttp {
    async fn get(
        &self,
        url: &Url,
        _: &[SocketAddr],
        _: usize,
    ) -> Result<HttpResponse, FailureReason> {
        self.calls.lock().unwrap().push(url.to_string());
        self.responses
            .get(url.as_str())
            .cloned()
            .unwrap_or(Err(FailureReason::SourceUnavailable))
    }
}

pub(crate) struct SourceSession<'a, H> {
    pub transport: &'a H,
    pub options: &'a QualificationOptions,
    pub requests: usize,
}
impl<H: HttpTransport> SourceSession<'_, H> {
    pub(crate) async fn read(
        &mut self,
        address: &str,
        visibility: &[crate::core::Visibility],
    ) -> Result<(Material, ExternalSource), FailureReason> {
        if visibility.is_empty()
            || visibility
                .iter()
                .any(|v| *v != crate::core::Visibility::Public)
        {
            return Err(FailureReason::RestrictedContext);
        }
        let mut url = Url::parse(address).map_err(|_| FailureReason::AddressRejected)?;
        loop {
            public_url(&url)?;
            if self.requests >= self.options.http_requests {
                return Err(FailureReason::HttpBudgetExhausted);
            }
            let timeout = Duration::from_millis(self.options.request_timeout_ms);
            let addresses = tokio::time::timeout(timeout, self.transport.prepare(&url))
                .await
                .map_err(|_| FailureReason::TimedOut)??;
            self.requests += 1;
            let response = tokio::time::timeout(
                timeout,
                self.transport
                    .get(&url, &addresses, self.options.max_source_bytes),
            )
            .await
            .map_err(|_| FailureReason::TimedOut)??;
            if (300..400).contains(&response.status) {
                url = url
                    .join(
                        response
                            .location
                            .as_deref()
                            .ok_or(FailureReason::SourceUnavailable)?,
                    )
                    .map_err(|_| FailureReason::AddressRejected)?;
                continue;
            }
            if response.status != 200 {
                return Err(FailureReason::SourceUnavailable);
            }
            if response.body.len() > self.options.max_source_bytes {
                return Err(FailureReason::TooLarge);
            }
            let format = if response.content_type.split(';').next().unwrap_or("").trim()
                == "application/pdf"
                || response.body.starts_with(b"%PDF-")
            {
                MaterialFormat::Pdf
            } else if matches!(
                response.content_type.split(';').next().unwrap_or("").trim(),
                "text/html" | "application/xhtml+xml"
            ) {
                MaterialFormat::Html
            } else {
                return Err(FailureReason::Unreadable);
            };
            let version = materials::version(&response.body);
            let id = format!(
                "source-{}",
                materials::version(format!("{url}|{version}").as_bytes())
            );
            return Ok((
                Material {
                    id: id.clone(),
                    visibility: crate::core::Visibility::Public,
                    format,
                    bytes: response.body,
                },
                ExternalSource {
                    material_id: id,
                    url: url.to_string(),
                    version,
                    retrieved_at: response.retrieved_at,
                },
            ));
        }
    }
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a != 0
                && a < 240
                && !(a == 100 && (64..128).contains(&b))
                && !(a == 198 && (18..20).contains(&b))
        }
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(|v| public_ip(IpAddr::V4(v)))
            .unwrap_or_else(|| {
                let octets = ip.octets();
                !ip.is_loopback()
                    && !ip.is_unspecified()
                    && !ip.is_multicast()
                    && !ip.is_unique_local()
                    && !ip.is_unicast_link_local()
                    && !(octets[..4] == [0x20, 0x01, 0x0d, 0xb8])
                    && (octets[0] & 0xe0) == 0x20
            }),
    }
}
pub(crate) fn public_url(url: &Url) -> Result<(), FailureReason> {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|p| p != 443)
    {
        return Err(FailureReason::AddressRejected);
    }
    match url.host() {
        Some(Host::Ipv4(ip)) if public_ip(IpAddr::V4(ip)) => Ok(()),
        Some(Host::Ipv6(ip)) if public_ip(IpAddr::V6(ip)) => Ok(()),
        Some(Host::Domain(host))
            if host.contains('.')
                && !host.ends_with(".localhost")
                && !host.ends_with(".local")
                && !host.ends_with('.') =>
        {
            Ok(())
        }
        _ => Err(FailureReason::AddressRejected),
    }
}

pub(crate) async fn pdf_text(
    bytes: &[u8],
    options: &QualificationOptions,
) -> Result<String, FailureReason> {
    let mut child = tokio::process::Command::new("pdftotext")
        .args(["-layout", "-enc", "UTF-8", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| FailureReason::Unreadable)?;
    let mut stdin = child.stdin.take().ok_or(FailureReason::Unreadable)?;
    let stdout = child.stdout.take().ok_or(FailureReason::Unreadable)?;
    let mut output = vec![];
    let limit = options.max_source_bytes as u64 + 1;
    let operation = async {
        let write = async move {
            stdin.write_all(bytes).await?;
            drop(stdin);
            Ok::<_, std::io::Error>(())
        };
        let read = async {
            stdout
                .take(limit)
                .read_to_end(&mut output)
                .await
                .map(|_| ())
        };
        tokio::try_join!(write, read).map_err(|_| FailureReason::Unreadable)?;
        if output.len() > options.max_source_bytes {
            return Err(FailureReason::TooLarge);
        }
        let status = child.wait().await.map_err(|_| FailureReason::Unreadable)?;
        if !status.success() {
            return Err(FailureReason::Unreadable);
        }
        let text = String::from_utf8(output).map_err(|_| FailureReason::Unreadable)?;
        if text.trim().is_empty() {
            return Err(FailureReason::Unreadable);
        }
        Ok(text)
    };
    tokio::time::timeout(Duration::from_millis(options.request_timeout_ms), operation)
        .await
        .map_err(|_| FailureReason::TimedOut)?
}
