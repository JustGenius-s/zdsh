use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs as _};
use std::rc::Rc;
use std::sync::Arc;
use std::{borrow::Cow, cell::RefCell};

use agent_client_protocol::schema::v2 as acp;
use anyhow::{Context as _, Result, bail};
use futures::{AsyncReadExt as _, FutureExt as _};
use gpui::{App, AppContext as _, Task};
use html_to_markdown::{TagHandler, convert_html_to_markdown, markdown};
use http_client::{AsyncBody, HttpClientWithUrl};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::SharedString;
use util::markdown::{MarkdownEscaped, MarkdownInlineCode};

use crate::{AgentTool, ToolCallEventStream, ToolInput};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
enum ContentType {
    Html,
    Plaintext,
    Json,
}

/// The maximum number of HTTP redirects the fetch tool will follow. Each hop's
/// host is checked against the private-address guard before being followed.
const MAX_REDIRECTS: usize = 20;

/// The outcome of a single (non-redirect-following) HTTP request.
enum FetchStep {
    /// The server responded with a redirect to this absolute URL. Its host must
    /// be checked before the redirect is followed.
    Redirect(String),
    /// A terminal response was received and converted to Markdown.
    Complete(String),
}

/// Prepends `https://` when the URL has no explicit HTTP(S) scheme, matching the
/// behavior the fetch tool has always had for user/model-supplied URLs.
fn normalize_url(url: &str) -> Cow<'_, str> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        Cow::Owned(format!("https://{url}"))
    } else {
        Cow::Borrowed(url)
    }
}

/// Fetches a URL and returns the content as Markdown.
///
/// Hosts that resolve only to loopback, private, or link-local addresses are
/// refused, and HTTP redirects are followed one hop at a time with each hop's
/// host checked the same way. When unsandboxed access has been granted, this
/// restriction is lifted, matching the terminal.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FetchToolInput {
    /// The URL to fetch.
    url: String,
}

pub struct FetchTool {
    http_client: Arc<HttpClientWithUrl>,
}

impl FetchTool {
    pub fn new(http_client: Arc<HttpClientWithUrl>) -> Self {
        Self { http_client }
    }

    /// Performs a single HTTP GET *without* following redirects, so the tool can
    /// check each hop's host before following it. Returns the redirect target when the server responds with a 3xx, or
    /// the final content converted to Markdown otherwise.
    async fn fetch_step(http_client: Arc<HttpClientWithUrl>, url: &str) -> Result<FetchStep> {
        let normalized = normalize_url(url);

        let mut response = http_client
            .get(&normalized, AsyncBody::default(), false)
            .await?;

        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get("location")
                .context("redirect response is missing a Location header")?
                .to_str()
                .context("redirect response has an invalid Location header")?;
            let target = url::Url::parse(&normalized)
                .with_context(|| format!("could not parse URL {normalized:?}"))?
                .join(location)
                .with_context(|| format!("invalid redirect target {location:?}"))?;
            anyhow::ensure!(
                matches!(target.scheme(), "http" | "https"),
                "refusing to follow redirect to non-HTTP(S) URL {target}"
            );
            return Ok(FetchStep::Redirect(target.to_string()));
        }

        let mut body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut body)
            .await
            .context("error reading response body")?;

        if status.is_client_error() {
            let text = String::from_utf8_lossy(body.as_slice());
            bail!("status error {}, response: {text:?}", status.as_u16());
        }

        let Some(content_type) = response.headers().get("content-type") else {
            bail!("missing Content-Type header");
        };
        let content_type = content_type
            .to_str()
            .context("invalid Content-Type header")?;

        let content_type = if content_type.starts_with("text/plain") {
            ContentType::Plaintext
        } else if content_type.starts_with("application/json") {
            ContentType::Json
        } else {
            ContentType::Html
        };

        let text = match content_type {
            ContentType::Html => {
                let mut handlers: Vec<TagHandler> = vec![
                    Rc::new(RefCell::new(markdown::WebpageChromeRemover)),
                    Rc::new(RefCell::new(markdown::ParagraphHandler)),
                    Rc::new(RefCell::new(markdown::HeadingHandler)),
                    Rc::new(RefCell::new(markdown::ListHandler)),
                    Rc::new(RefCell::new(markdown::TableHandler::new())),
                    Rc::new(RefCell::new(markdown::StyledTextHandler)),
                ];
                if normalized.contains("wikipedia.org") {
                    use html_to_markdown::structure::wikipedia;

                    handlers.push(Rc::new(RefCell::new(wikipedia::WikipediaChromeRemover)));
                    handlers.push(Rc::new(RefCell::new(wikipedia::WikipediaInfoboxHandler)));
                    handlers.push(Rc::new(
                        RefCell::new(wikipedia::WikipediaCodeHandler::new()),
                    ));
                } else {
                    handlers.push(Rc::new(RefCell::new(markdown::CodeHandler)));
                }

                convert_html_to_markdown(&body[..], &mut handlers)?
            }
            ContentType::Plaintext => std::str::from_utf8(&body)?.to_owned(),
            ContentType::Json => {
                let json: serde_json::Value = serde_json::from_slice(&body)?;

                format!("```json\n{}\n```", serde_json::to_string_pretty(&json)?)
            }
        };

        Ok(FetchStep::Complete(text))
    }
}

/// Resolve the host of `url` and confirm it doesn't point into loopback /
/// private / link-local space. Returns an error (including "resolves only to
/// loopback/private/link-local addresses") that aborts the fetch.
///
/// DNS resolution blocks, so callers should run this off the foreground thread.
/// See the caller for why this is a gate rather than a full resolve-to-connect
/// pin.
fn verify_host_not_forbidden(url: &str) -> Result<()> {
    let normalized = normalize_url(url);
    let parsed =
        url::Url::parse(&normalized).with_context(|| format!("could not parse URL {url:?}"))?;
    let host = parsed
        .host()
        .with_context(|| format!("URL {url:?} has no host to reach"))?;
    // Default to the scheme's port when the URL omits one; resolution needs a
    // port but the value doesn't affect which IPs a host resolves to.
    let port = parsed
        .port_or_known_default()
        .unwrap_or(if parsed.scheme() == "http" { 80 } else { 443 });

    let addresses: Vec<SocketAddr> = match host {
        url::Host::Domain(domain) => (domain, port)
            .to_socket_addrs()
            .with_context(|| format!("resolving {domain}:{port}"))?
            .collect(),
        url::Host::Ipv4(ip) => vec![SocketAddr::new(ip.into(), port)],
        url::Host::Ipv6(ip) => vec![SocketAddr::new(ip.into(), port)],
    };
    if addresses.is_empty() {
        bail!("{host}:{port} did not resolve to any address");
    }
    if addresses
        .iter()
        .all(|address| is_forbidden_ip(address.ip()))
    {
        bail!(
            "{host} resolves only to loopback/private/link-local addresses, which fetch can \
             only reach once unsandboxed access has been granted (for example, via a terminal \
             command that requests it)"
        );
    }
    Ok(())
}

/// Whether a resolved address is in loopback / private / link-local space,
/// which fetch must not reach on the model's behalf.
fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_forbidden_ipv4(v4);
            }
            v6.is_loopback()
                || v6.is_unspecified()
                // Link-local (fe80::/10) and unique-local (fc00::/7); the
                // dedicated `is_unicast_link_local` / `is_unique_local`
                // methods are not yet stable.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

fn is_forbidden_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // includes 169.254.169.254 cloud metadata
        || ip.is_unspecified()
        || ip.is_broadcast()
        // Shared address space (RFC 6598, 100.64.0.0/10): CGNAT, and notably
        // Tailscale-style overlay networks.
        || (octets[0] == 100 && (octets[1] & 0xc0) == 64)
}

impl AgentTool for FetchTool {
    type Input = FetchToolInput;
    type Output = String;

    const NAME: &'static str = "fetch";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Fetch
    }

    fn allow_in_restricted_mode() -> bool {
        false
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => format!("Fetch {}", MarkdownEscaped(&input.url)).into(),
            Err(_) => "Fetch URL".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        let http_client = self.http_client.clone();
        cx.spawn(async move |cx| {
            let input: FetchToolInput = input.recv().await.map_err(|e| e.to_string())?;

            // First, the standard tool-permission gate (honors the fetch tool's
            // allow/deny/confirm rules).
            let authorize = cx.update(|cx| {
                let context =
                    crate::ToolPermissionContext::new(Self::NAME, vec![input.url.clone()]);

                event_stream.authorize(
                    format!("Fetch {}", MarkdownInlineCode(&input.url)),
                    context,
                    cx,
                )
            });
            futures::select! {
                result = authorize.fuse() => result.map_err(|e| e.to_string())?,
                _ = event_stream.cancelled_by_user().fuse() => {
                    return Err("Fetch cancelled by user".to_string());
                }
            };

            // Then, unless unsandboxed access is already in effect, refuse hosts
            // that resolve into loopback / private / link-local space, so a
            // fetched page can't steer the model into the local machine, the
            // LAN, or a cloud metadata endpoint. When the thread has full access
            // or unsandboxed access has been granted, the terminal already runs
            // without isolation, so we drop the check too; that's also how local
            // services become reachable.
            //
            // The check applies to every redirect hop as well as the initial
            // URL, so a public host can't 30x-redirect the fetch into private
            // space. We disable the HTTP client's own redirect following and
            // re-run the check for each hop before requesting it.
            let unsandboxed = event_stream.unsandboxed_access_granted();

            let mut current_url = input.url.clone();
            let mut redirects = 0;
            let text = loop {
                if !unsandboxed {
                    // NOTE: this is a gate, not a full pin. `HttpClientWithUrl`
                    // resolves the hostname again when it connects, so a DNS
                    // answer that flips between this check and that connect could
                    // still slip through. Closing that residual window would
                    // require the HTTP client to connect to a pre-vetted IP
                    // rather than re-resolving; until then this blocks the
                    // realistic case of a stably resolving host that points at
                    // forbidden space.
                    let verify_task = cx.background_spawn({
                        let url = current_url.clone();
                        async move { verify_host_not_forbidden(&url) }
                    });
                    futures::select! {
                        result = verify_task.fuse() => result.map_err(|e| e.to_string())?,
                        _ = event_stream.cancelled_by_user().fuse() => {
                            return Err("Fetch cancelled by user".to_string());
                        }
                    };
                }

                let fetch_task = cx.background_spawn({
                    let http_client = http_client.clone();
                    let url = current_url.clone();
                    async move { Self::fetch_step(http_client, &url).await }
                });

                let step = futures::select! {
                    result = fetch_task.fuse() => result.map_err(|e| e.to_string())?,
                    _ = event_stream.cancelled_by_user().fuse() => {
                        return Err("Fetch cancelled by user".to_string());
                    }
                };

                match step {
                    FetchStep::Complete(text) => break text,
                    FetchStep::Redirect(target) => {
                        redirects += 1;
                        if redirects > MAX_REDIRECTS {
                            return Err(format!(
                                "exceeded the maximum of {MAX_REDIRECTS} redirects"
                            ));
                        }
                        current_url = target;
                    }
                }
            };

            if text.trim().is_empty() {
                return Err("no textual content found".to_string());
            }
            Ok(text)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These use IP-literal URLs, which "resolve" to themselves, so the SSRF gate
    // is exercised without depending on real DNS.

    #[test]
    fn verify_host_rejects_loopback_literal() {
        let error = verify_host_not_forbidden("http://127.0.0.1/internal")
            .expect_err("loopback must be refused");
        assert!(
            error.to_string().contains("loopback"),
            "error should explain the forbidden range, got: {error}"
        );
    }

    #[test]
    fn verify_host_rejects_private_and_metadata_literals() {
        for url in [
            "http://10.0.0.5/",
            "https://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/", // cloud metadata
            "http://[::1]/",
        ] {
            assert!(
                verify_host_not_forbidden(url).is_err(),
                "expected {url} to be refused as a forbidden destination"
            );
        }
    }

    #[test]
    fn verify_host_allows_public_literal() {
        verify_host_not_forbidden("https://93.184.215.14/")
            .expect("a public address must be allowed through the gate");
        verify_host_not_forbidden("https://[2606:2800:220:1:248:1893:25c8:1946]/")
            .expect("a public IPv6 address must be allowed through the gate");
    }

    #[test]
    fn forbidden_ip_covers_v4_ranges() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254", // cloud metadata
            "0.0.0.0",
            "255.255.255.255",
            "100.64.0.1", // CGNAT / Tailscale
        ] {
            assert!(
                is_forbidden_ip(ip.parse().unwrap()),
                "expected {ip} to be forbidden"
            );
        }
    }

    #[test]
    fn forbidden_ip_covers_v6_ranges() {
        for ip in [
            "::1",              // loopback
            "::",               // unspecified
            "fe80::1",          // link-local
            "fc00::1",          // unique-local
            "::ffff:127.0.0.1", // IPv4-mapped loopback
            "::ffff:10.0.0.1",  // IPv4-mapped private
        ] {
            assert!(
                is_forbidden_ip(ip.parse().unwrap()),
                "expected {ip} to be forbidden"
            );
        }
    }

    #[test]
    fn public_ips_are_allowed() {
        for ip in [
            "93.184.215.14",
            "8.8.8.8",
            "2606:2800:220:1:248:1893:25c8:1946",
        ] {
            assert!(
                !is_forbidden_ip(ip.parse().unwrap()),
                "expected {ip} to be allowed"
            );
        }
    }
}
