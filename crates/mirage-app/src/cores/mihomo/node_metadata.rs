//! Node transport labels from the active config and its provider caches.
use super::{compose, controller::ProxyGroupView};
use crate::storage::layout::Layout;
use serde_yaml_ng::Value;
use std::collections::HashMap;
use std::path::Path;

pub async fn apply(groups: &mut [ProxyGroupView], layout: &Layout, service: bool) {
    let Some(config) = read_config(&layout.run_config()).await else {
        return;
    };
    let mut transports = HashMap::new();
    collect(config.get("proxies"), &mut transports);
    let home = if service {
        mirage_ipc::known_folder::service_data()
            .ok()
            .map(|path| path.join("tun-home"))
    } else {
        Some(layout.core_home.clone())
    };
    if let Some(providers) = config.get("proxy-providers").and_then(Value::as_mapping) {
        for provider in providers.values() {
            collect(provider.get("payload"), &mut transports);
            if let (Some(home), Some(path)) = (&home, provider.get("path").and_then(Value::as_str))
                && let Some(cached) = read_config(&home.join(path)).await
            {
                collect(cached.get("proxies"), &mut transports);
            }
        }
    }
    for node in groups.iter_mut().flat_map(|group| &mut group.nodes) {
        if let Some(transport) = transports.get(&node.name) {
            node.transport.clone_from(transport);
        }
    }
}

async fn read_config(path: &Path) -> Option<Value> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    compose::parse(&text).ok()
}

fn collect(proxies: Option<&Value>, transports: &mut HashMap<String, String>) {
    let Some(proxies) = proxies.and_then(Value::as_sequence) else {
        return;
    };
    for proxy in proxies {
        let Some(name) = proxy.get("name").and_then(Value::as_str) else {
            continue;
        };
        transports.insert(name.to_owned(), transport(proxy));
    }
}

fn transport(proxy: &Value) -> String {
    let network = proxy.get("network").and_then(Value::as_str).unwrap_or("");
    if !network.is_empty() {
        return match network.to_ascii_lowercase().as_str() {
            "grpc" => "gRPC".into(),
            "httpupgrade" => "HTTPUpgrade".into(),
            _ => network.to_ascii_uppercase(),
        };
    }
    let kind = proxy.get("type").and_then(Value::as_str).unwrap_or("");
    if matches!(kind, "hysteria" | "hysteria2" | "tuic" | "wireguard") {
        return "UDP".into();
    }
    if proxy.get("plugin").and_then(Value::as_str) == Some("v2ray-plugin")
        && proxy
            .get("plugin-opts")
            .and_then(|options| options.get("mode"))
            .and_then(Value::as_str)
            == Some("websocket")
    {
        return "WS".into();
    }
    "TCP".into()
}
