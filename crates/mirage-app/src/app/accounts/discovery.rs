//! Provider host discovery for diagnostics.
use crate::app::{AppCore, Internal};
use crate::domain::error::AppResult;
use crate::providers::ziyoumao::bootstrap;

impl AppCore {
    /// Diagnostics (ZIYOUMAO.md §10 step 1): candidate hosts and results.
    pub(in crate::app) fn probe_provider_hosts(&mut self) {
        if self.view.probing_hosts {
            return;
        }
        self.view.probing_hosts = true;
        let layout = self.layout.clone();
        let egress = self.provider_egress();
        self.spawn(
            async move { Internal::HostsProbed(bootstrap::discover(&layout, egress).await) },
        );
    }

    pub(in crate::app) fn on_hosts_probed(&mut self, result: AppResult<bootstrap::Discovery>) {
        self.view.probing_hosts = false;
        self.view.provider_hosts = match result {
            Ok(d) => {
                let mut lines = vec![if d.from_cache {
                    "入口文档不可用，使用缓存的主机列表".to_owned()
                } else {
                    "入口文档：已读取".to_owned()
                }];
                lines.extend(d.probes.iter().map(|p| match &p.result {
                    Ok(()) => format!("{}：可用", p.host),
                    Err(e) => format!("{}：{e}", p.host),
                }));
                lines.push(match &d.selected {
                    Some((base, _)) => format!("选用：{}", bootstrap::origin(base)),
                    None => "没有可用的主机".into(),
                });
                lines
            }
            Err(e) => vec![format!("主机发现失败：{}", e.user_message())],
        };
    }
}
