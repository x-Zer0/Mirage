//! Mihomo REST controller client (DESIGN.md §10.5). Loopback only, no proxy.

use crate::domain::error::{AppError, AppErrorKind, AppResult};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::{Client, Method, Url};
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

const STAGE: &str = "mihomo.controller";
const TIMEOUT: Duration = Duration::from_secs(5);
const RELOAD_TIMEOUT: Duration = Duration::from_secs(30);
pub const DELAY_TEST_URL: &str = "https://cp.cloudflare.com/generate_204";
const DELAY_TIMEOUT_MS: u32 = 5000;

#[derive(Clone)]
pub struct Controller {
    base: Url,
    http: Client,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeView {
    pub name: String,
    /// Latest delay in ms; `Some(0)` means the last test timed out.
    pub delay: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyGroupView {
    pub name: String,
    pub kind: String,
    pub now: String,
    pub selectable: bool,
    pub nodes: Vec<NodeView>,
}

#[derive(Deserialize)]
struct ProxiesResponse {
    proxies: HashMap<String, ProxyEntry>,
}

#[derive(Deserialize)]
struct ProxyEntry {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    now: Option<String>,
    #[serde(default)]
    all: Option<Vec<String>>,
    #[serde(default)]
    history: Vec<DelayRecord>,
}

#[derive(Deserialize)]
struct DelayRecord {
    delay: u32,
}

impl Controller {
    pub fn new(addr: SocketAddr, secret: &str) -> AppResult<Self> {
        let mut headers = HeaderMap::new();
        let mut auth = HeaderValue::from_str(&format!("Bearer {secret}"))
            .map_err(|e| AppError::internal(STAGE, e))?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
        let http = Client::builder()
            .no_proxy()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .build()
            .map_err(|e| AppError::internal(STAGE, e.without_url()))?;
        let base =
            Url::parse(&format!("http://{addr}/")).map_err(|e| AppError::internal(STAGE, e))?;
        Ok(Self { base, http })
    }

    pub async fn version(&self) -> AppResult<String> {
        #[derive(Deserialize)]
        struct Version {
            version: String,
        }
        let v: Version = self.json(Method::GET, &["version"], None).await?;
        Ok(v.version)
    }

    /// Groups ordered as declared in the config (via GLOBAL's member list).
    pub async fn groups(&self) -> AppResult<Vec<ProxyGroupView>> {
        let resp: ProxiesResponse = self.json(Method::GET, &["proxies"], None).await?;
        let mut order: Vec<String> = resp
            .proxies
            .get("GLOBAL")
            .and_then(|g| g.all.clone())
            .unwrap_or_default();
        let mut rest: Vec<&String> = resp.proxies.keys().filter(|k| !order.contains(k)).collect();
        rest.sort();
        order.extend(rest.into_iter().cloned());

        let groups = order
            .iter()
            .filter(|name| name.as_str() != "GLOBAL")
            .filter_map(|name| {
                let entry = resp.proxies.get(name)?;
                let members = entry.all.as_ref()?;
                Some(ProxyGroupView {
                    name: name.clone(),
                    kind: entry.kind.clone(),
                    now: entry.now.clone().unwrap_or_default(),
                    selectable: entry.kind == "Selector",
                    nodes: members
                        .iter()
                        .map(|m| NodeView {
                            name: m.clone(),
                            delay: resp
                                .proxies
                                .get(m)
                                .and_then(|p| p.history.last())
                                .map(|h| h.delay),
                        })
                        .collect(),
                })
            })
            .collect();
        Ok(groups)
    }

    pub async fn select(&self, group: &str, node: &str) -> AppResult<()> {
        let body = serde_json::json!({ "name": node });
        self.send(Method::PUT, &["proxies", group], Some(body), None)
            .await
    }

    pub async fn test_group_delay(&self, group: &str) -> AppResult<()> {
        let mut url = self.url(&["group", group, "delay"])?;
        url.query_pairs_mut()
            .append_pair("url", DELAY_TEST_URL)
            .append_pair("timeout", &DELAY_TIMEOUT_MS.to_string());
        let timeout = Duration::from_millis(u64::from(DELAY_TIMEOUT_MS)) + TIMEOUT;
        let resp = self
            .http
            .get(url)
            .timeout(timeout)
            .send()
            .await
            .map_err(map_reqwest)?;
        // 504/503 here only means every node timed out; history is still updated.
        if resp.status().is_success() || resp.status().is_server_error() {
            Ok(())
        } else {
            Err(AppError::new(
                AppErrorKind::Http(resp.status().as_u16()),
                STAGE,
                "测速请求失败",
            ))
        }
    }

    pub async fn close_all_connections(&self) -> AppResult<()> {
        self.send(Method::DELETE, &["connections"], None, None)
            .await
    }

    /// Hot reload from the run config on disk (§10.5, §10.6). `force` makes
    /// Mihomo apply even when it thinks nothing changed. Provider downloads may
    /// happen during the reload, hence the longer timeout.
    pub async fn reload(&self, config: &std::path::Path) -> AppResult<()> {
        let mut url = self.url(&["configs"])?;
        url.query_pairs_mut().append_pair("force", "true");
        let body = serde_json::json!({ "path": config.to_string_lossy(), "payload": "" });
        let resp = self
            .http
            .put(url)
            .json(&body)
            .timeout(RELOAD_TIMEOUT)
            .send()
            .await
            .map_err(map_reqwest)?;
        if resp.status().is_success() {
            return Ok(());
        }
        let code = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        Err(AppError::new(
            AppErrorKind::Http(code),
            STAGE,
            format!(
                "热重载被拒绝：{}",
                text.chars().take(200).collect::<String>()
            ),
        ))
    }

    async fn json<T: for<'de> Deserialize<'de>>(
        &self,
        method: Method,
        path: &[&str],
        body: Option<serde_json::Value>,
    ) -> AppResult<T> {
        let resp = self.request(method, path, body, None).await?;
        resp.json().await.map_err(|e| {
            AppError::new(
                AppErrorKind::CoreNotReady,
                STAGE,
                e.without_url().to_string(),
            )
        })
    }

    async fn send(
        &self,
        method: Method,
        path: &[&str],
        body: Option<serde_json::Value>,
        timeout: Option<Duration>,
    ) -> AppResult<()> {
        self.request(method, path, body, timeout).await.map(drop)
    }

    async fn request(
        &self,
        method: Method,
        path: &[&str],
        body: Option<serde_json::Value>,
        timeout: Option<Duration>,
    ) -> AppResult<reqwest::Response> {
        let mut req = self.http.request(method, self.url(path)?);
        if let Some(body) = body {
            req = req.json(&body);
        }
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let resp = req.send().await.map_err(map_reqwest)?;
        if resp.status().is_success() {
            Ok(resp)
        } else {
            let code = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            Err(AppError::new(
                AppErrorKind::Http(code),
                STAGE,
                text.chars().take(200).collect::<String>(),
            ))
        }
    }

    /// Path segments are percent-encoded individually (node names may hold `/`, emoji).
    fn url(&self, segments: &[&str]) -> AppResult<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| AppError::internal(STAGE, "controller base URL"))?
            .clear()
            .extend(segments);
        Ok(url)
    }
}

fn map_reqwest(e: reqwest::Error) -> AppError {
    let kind = if e.is_timeout() {
        AppErrorKind::Timeout
    } else {
        AppErrorKind::CoreNotReady
    };
    AppError::new(kind, STAGE, e.without_url().to_string())
}
