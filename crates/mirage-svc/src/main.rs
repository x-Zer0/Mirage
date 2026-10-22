//! `mirage-svc` (DESIGN.md §11.7).
//!
//! Modes:
//! * `install --client <path> --user-sid <sid>` — elevated; copies the helper
//!   and core into `%ProgramFiles%\Mirage\mirage-svc`, locks ACLs, creates and
//!   starts the service.
//! * `uninstall` — elevated; stops and removes the service and its trees.
//! * no arguments — SCM entry (`StartServiceCtrlDispatcherW`).

mod acl;
mod core;
mod identity;
mod install;
mod log;
mod paths;
mod pipe;
mod process;
mod record;
mod service;

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    // The reload request is plain loopback HTTP, but in a workspace build
    // reqwest is unified with the app's `rustls-no-provider` feature and
    // building a client without a provider panics (abort kills the service
    // and its TUN core). Install the same ring provider the app uses.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("install") => {
            let client = option(&args, "--client")
                .map(PathBuf::from)
                .ok_or_else(|| anyhow::anyhow!("缺少 --client <Mirage.exe 路径>"))?;
            let user_sid = match option(&args, "--user-sid") {
                Some(sid) if !sid.is_empty() => sid,
                _ => identity::current_user_sid()?,
            };
            let core = option(&args, "--core-source")
                .map(PathBuf::from)
                .unwrap_or_else(|| client.with_file_name("core"));
            install::install(&client, &user_sid, &core)?;
            println!("mirage-svc 已安装");
            Ok(())
        }
        Some("uninstall") => {
            install::uninstall()?;
            println!("mirage-svc 已卸载");
            Ok(())
        }
        Some(other) => {
            anyhow::bail!("未知参数 {other}；用法：install | uninstall")
        }
        None => service::dispatch(),
    }
}

fn option(args: &[String], name: &str) -> Option<String> {
    let index = args.iter().position(|a| a == name)?;
    args.get(index + 1).cloned()
}
