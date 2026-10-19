//! Host discovery (ZIYOUMAO.md §4): public Base64 JSON → candidate hosts →
//! `GET /guest/comm/config` without a token → API base. The last good host
//! list is cached without any token.

use super::STAGE;
use super::api::{self, Egress};
use crate::domain::error::{AppError, AppErrorKind, AppResult};
use crate::domain::model::{Timestamp, now};
use crate::net::{self, EgressRoute};
use crate::storage::layout::Layout;
use crate::storage::meta;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::header::ACCEPT;
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const ENTRY: &str = "https://flmain-1326556767.cos.ap-guangzhou.myqcloud.com/ConFigOss.json";
/// §4.2: at most two hosts are probed at the same time.
const PROBE_PARALLEL: usize = 2;
const CACHE_FILE: &str = "bootstrap.json";

#[derive(Deserialize)]
struct BootstrapDoc {
    hosts: Vec<String>,
}

/// `providers-cache/ziyoumao/bootstrap.json`; never holds a token.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapCache {
    pub hosts: Vec<String>,
    pub selected: Option<String>,
    pub fetched_at: Timestamp,
}

/// Result of probing one host, shown on the diagnostics page.
#[derive(Debug, Clone)]
pub struct HostProbe {
    /// `https://host` (no path, no token).
    pub host: String,
    pub result: Result<(), String>,
}

/// Public settings of the selected host that matter for login (§6.3).
#[derive(Debug, Clone, Copy)]
pub struct CommConfig {
    pub recaptcha: bool,
    /// Route the probe succeeded on; the login POST uses exactly this one
    /// so it is never sent twice (§8).
    pub route: EgressRoute,
}

#[derive(Debug, Clone)]
pub struct Discovery {
    /// API base `<host>/api/v1/` of the first host that answered properly.
    pub selected: Option<(Url, CommConfig)>,
    pub probes: Vec<HostProbe>,
    /// Hosts came from the cache because the entry document failed.
    pub from_cache: bool,
    /// Every accepted candidate base, selected or not.
    pub candidates: Vec<Url>,
}

/// Downloads the entry, falls back to the cache, then probes the hosts.
pub async fn discover(layout: &Layout, egress: Egress) -> AppResult<Discovery> {
    let cache_path = layout.provider_cache(super::PROVIDER_ID).join(CACHE_FILE);
    let (hosts, from_cache) = match fetch_entry(egress).await {
        Ok(hosts) => (hosts, false),
        Err(e) => {
            tracing::warn!(error = %e, "ziyoumao bootstrap failed, using cache");
            let cached: BootstrapCache = meta::load(&cache_path, STAGE)?;
            if cached.hosts.is_empty() {
                return Err(e.append("（也没有可用的主机缓存）"));
            }
            (cached.hosts, true)
        }
    };
    let candidates = candidates(&hosts);
    let mut probes: Vec<HostProbe> = hosts
        .iter()
        .filter(|h| !candidates.iter().any(|c| same_host(c, h)))
        .map(|h| HostProbe {
            host: display_host(h),
            result: Err("已跳过：需要 https:// 域名地址".into()),
        })
        .collect();
    let checked = probe_all(&candidates, egress).await;
    let selected = checked
        .iter()
        .find_map(|(base, r)| r.as_ref().ok().map(|c| (base.clone(), *c)));
    probes.extend(checked.iter().map(|(base, r)| HostProbe {
        host: origin(base),
        result: r.as_ref().map(|_| ()).map_err(|e| e.user_message()),
    }));
    let mut cache = BootstrapCache {
        hosts,
        selected: selected.as_ref().map(|(b, _)| origin(b)),
        fetched_at: now(),
    };
    if selected.is_some() {
        save_cache(&cache_path, &mut cache);
    }
    Ok(Discovery {
        selected,
        probes,
        from_cache,
        candidates,
    })
}

/// The selected base or an error listing every host's failure (§4.2).
pub fn require_selected(d: &Discovery) -> AppResult<(Url, CommConfig)> {
    d.selected.clone().ok_or_else(|| {
        let detail = d
            .probes
            .iter()
            .map(|p| match &p.result {
                Ok(()) => format!("{} 可用", p.host),
                Err(e) => format!("{}：{e}", p.host),
            })
            .collect::<Vec<_>>()
            .join("；");
        AppError::new(
            AppErrorKind::Network,
            STAGE,
            format!("没有可用的自由猫服务地址（{detail}）"),
        )
    })
}

/// Debug builds may point the entry at a local test server.
fn entry_url() -> AppResult<Url> {
    let custom = cfg!(debug_assertions)
        .then(|| std::env::var("MIRAGE_ZIYOUMAO_BOOTSTRAP").ok())
        .flatten();
    Url::parse(custom.as_deref().unwrap_or(ENTRY)).map_err(|e| AppError::internal(STAGE, e))
}

async fn fetch_entry(egress: Egress) -> AppResult<Vec<String>> {
    let url = entry_url()?;
    let body = net::with_egress(egress.policy, egress.core_port, |route| {
        fetch_entry_once(&url, route)
    })
    .await?;
    let text = std::str::from_utf8(&body)
        .map_err(|_| api::compat("入口文档不是文本"))?
        .trim();
    let decoded = STANDARD
        .decode(text)
        .map_err(|_| api::compat("入口文档不是 Base64"))?;
    let doc: BootstrapDoc =
        serde_json::from_slice(&decoded).map_err(|_| api::compat("入口文档不是预期的 JSON"))?;
    if doc.hosts.is_empty() {
        return Err(api::compat("入口文档中没有主机"));
    }
    tracing::info!(hosts = doc.hosts.len(), "ziyoumao bootstrap loaded");
    Ok(doc.hosts)
}

async fn fetch_entry_once(url: &Url, route: EgressRoute) -> AppResult<Vec<u8>> {
    let client = net::client(&net::BOOTSTRAP, route)?;
    let resp = client
        .get(url.clone())
        .header(ACCEPT, "*/*")
        .send()
        .await
        .map_err(net::map_error)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::new(
            AppErrorKind::Http(status.as_u16()),
            STAGE,
            format!("入口返回 HTTP {}", status.as_u16()),
        ));
    }
    net::read_bounded(resp, net::BOOTSTRAP.max_body).await
}

/// §4.2: `https://<domain>` without userinfo, query or fragment. IP literals
/// are skipped — TLS verification is never relaxed to use them.
fn candidates(hosts: &[String]) -> Vec<Url> {
    const MAX_CANDIDATES: usize = 16;
    let mut out: Vec<Url> = Vec::new();
    for host in hosts {
        if out.len() >= MAX_CANDIDATES {
            break;
        }
        let Ok(url) = Url::parse(host.trim()) else {
            continue;
        };
        if net::check_domain_url(&url, false).is_err() {
            continue;
        }
        let Ok(base) = Url::parse(&format!("{}/api/v1/", origin(&url))) else {
            continue;
        };
        if !out.contains(&base) {
            out.push(base);
        }
    }
    out
}

/// Probes every candidate (two at a time) and returns results in list order.
async fn probe_all(bases: &[Url], egress: Egress) -> Vec<(Url, AppResult<CommConfig>)> {
    let permits = Arc::new(Semaphore::new(PROBE_PARALLEL));
    let mut set = JoinSet::new();
    for (i, base) in bases.iter().cloned().enumerate() {
        let permits = permits.clone();
        set.spawn(async move {
            let _permit = permits.acquire_owned().await;
            let result = probe(&base, egress).await;
            (i, base, result)
        });
    }
    let mut out = Vec::with_capacity(bases.len());
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(item) => out.push(item),
            Err(e) => tracing::warn!(error = %e, "host probe task failed"),
        }
    }
    out.sort_by_key(|(i, ..)| *i);
    out.into_iter().map(|(_, b, r)| (b, r)).collect()
}

/// `GET guest/comm/config`: 200 + JSON + `data` object (§4.2). No retries
/// here; a failing host is simply not selected.
async fn probe(base: &Url, egress: Egress) -> AppResult<CommConfig> {
    let url = api::endpoint(base, "guest/comm/config")?;
    let (route, raw) = net::with_egress(egress.policy, egress.core_port, |route| {
        let url = &url;
        async move {
            api::send(Method::GET, url, None, None, route)
                .await
                .map(|raw| (route, raw))
        }
    })
    .await?;
    let data = api::data_of(&raw)?;
    let Some(obj) = data.as_object() else {
        return Err(api::compat("公共配置的 data 不是对象"));
    };
    let recaptcha = obj
        .get("is_recaptcha")
        .is_some_and(|v| v.as_bool().unwrap_or(false) || v.as_i64().is_some_and(|n| n != 0));
    tracing::info!(
        host = base.host_str().unwrap_or_default(),
        recaptcha,
        via_core = matches!(route, EgressRoute::ViaLocalCore(_)),
        "ziyoumao host ok"
    );
    Ok(CommConfig { recaptcha, route })
}

fn save_cache(path: &std::path::Path, cache: &mut BootstrapCache) {
    let result = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .map_err(|e| AppError::storage(STAGE, e))
        .and_then(|()| meta::save(path, cache, STAGE));
    if let Err(e) = result {
        tracing::warn!(error = %e, "bootstrap cache not written");
    }
}

/// `https://host[:port]` of a URL.
pub fn origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Whether `base` is the API base belonging to the trusted origin `host`.
pub fn same_host(base: &Url, host: &str) -> bool {
    Url::parse(host.trim()).is_ok_and(|h| h.origin() == base.origin())
}

/// Host part only, for showing skipped entries without paths or queries.
fn display_host(raw: &str) -> String {
    Url::parse(raw.trim())
        .ok()
        .and_then(|u| u.host_str().map(|h| format!("{}://{h}", u.scheme())))
        .unwrap_or_else(|| "（无法解析的地址）".into())
}
