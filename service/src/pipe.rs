//! One authenticated owner connection. Disconnect/lease expiry stops its core.
use crate::{
    core::{self, CoreShell},
    identity, log, record, service,
};
use anyhow::Context;
use mirage_ipc::protocol::{self, ErrorKind, PIPE_NAME, PROTOCOL_VERSION, Request, Response};
use std::{io, os::windows::io::AsRawHandle, sync::Arc, time::Duration};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows::{
    Win32::{
        Foundation::{HANDLE, HLOCAL, LocalFree},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        System::Pipes::GetNamedPipeClientProcessId,
    },
    core::PCWSTR,
};

pub async fn serve(shell: Arc<CoreShell>) -> anyhow::Result<()> {
    let record = record::load()?;
    let mut pipe = create(&record.user_sid, true)?;
    while !service::stopping() {
        tokio::select! {
            result = pipe.connect() => result?,
            _ = service::wait_stop() => break,
        }
        let mut pid = 0;
        // SAFETY: connected server handle with valid output pointer.
        let allowed =
            unsafe { GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }.is_ok()
                && identity::identity(pid)
                    .is_some_and(|id| identity::same_path(&id.image, &record.client_exe));
        if allowed {
            log::info(&format!("client connected pid={pid}"));
            if let Err(error) = session(&mut pipe, shell.clone()).await {
                log::warn(&format!("owner connection ended: {error:#}"));
            }
            shell
                .stop()
                .map_err(|(_, detail)| anyhow::anyhow!(detail))?;
        } else {
            log::warn(&format!("client rejected pid={pid}"));
        }
        // Mio retains pending reads and errors after DisconnectNamedPipe.
        // Keep the name reserved while replacing the entire async pipe instance.
        let next = create(&record.user_sid, false)?;
        let _ = pipe.disconnect();
        pipe = next;
    }
    shell
        .stop()
        .map_err(|(_, detail)| anyhow::anyhow!(detail))?;
    Ok(())
}

async fn session(pipe: &mut NamedPipeServer, shell: Arc<CoreShell>) -> anyhow::Result<()> {
    let hello = tokio::time::timeout(Duration::from_secs(5), protocol::receive(pipe))
        .await
        .context("Hello timeout")?
        .map_err(frame_error)
        .context("receive Hello")?;
    let Request::Hello {
        protocol,
        app_version,
    } = hello
    else {
        anyhow::bail!("Hello required");
    };
    let compatible = protocol == PROTOCOL_VERSION && app_version == env!("CARGO_PKG_VERSION");
    protocol::send(
        pipe,
        &Response::Hello {
            protocol: PROTOCOL_VERSION,
            service_version: env!("CARGO_PKG_VERSION").into(),
            status: shell.status(),
        },
    )
    .await
    .map_err(frame_error)
    .context("send Hello")?;
    loop {
        let request: Request = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(15), protocol::receive(pipe)) => result.context("owner lease expired")?.map_err(frame_error).context("receive request")?,
            _ = service::wait_stop() => return Ok(()),
        };
        let needs_version = matches!(
            request,
            Request::StartCore { .. } | Request::ReloadCore { .. }
        );
        let response = if needs_version && !compatible {
            Response::Err {
                kind: ErrorKind::VersionMismatch,
                detail: ErrorKind::VersionMismatch.label().into(),
            }
        } else {
            dispatch(shell.clone(), request).await
        };
        tokio::time::timeout(Duration::from_secs(5), protocol::send(pipe, &response))
            .await
            .context("response timeout")?
            .map_err(frame_error)
            .context("send response")?;
    }
}

// Never include malformed JSON values (which may contain credentials) in logs.
fn frame_error(error: protocol::FrameError) -> anyhow::Error {
    match error {
        protocol::FrameError::Malformed(_) => anyhow::anyhow!("invalid pipe frame"),
        other => anyhow::anyhow!(other),
    }
}

async fn dispatch(shell: Arc<CoreShell>, request: Request) -> Response {
    let result = match request {
        Request::ReloadCore { config_yaml } => {
            let prepared =
                tokio::task::spawn_blocking(move || shell.prepare_reload(&config_yaml)).await;
            match prepared {
                Ok(Ok(prepared)) => core::reload(prepared).await.map(|()| Response::Ok),
                Ok(Err(e)) => Err(e),
                Err(_) => Err((ErrorKind::Io, "重载任务失败".into())),
            }
        }
        request => {
            match tokio::task::spawn_blocking(move || dispatch_sync(&shell, request)).await {
                Ok(result) => result,
                Err(_) => Err((ErrorKind::Io, "服务任务失败".into())),
            }
        }
    };
    result.unwrap_or_else(|(kind, detail)| Response::Err { kind, detail })
}

fn dispatch_sync(shell: &CoreShell, request: Request) -> Result<Response, (ErrorKind, String)> {
    match request {
        Request::StartCore {
            config_yaml,
            ext_ctl_port,
            secret,
            profile_id,
        } => {
            let pid = shell.start(&config_yaml, ext_ctl_port, &secret, &profile_id)?;
            Ok(Response::Started {
                pid,
                status: shell.status(),
            })
        }
        Request::StopCore => {
            shell.stop()?;
            Ok(Response::Stopped)
        }
        Request::Status => Ok(Response::Status(shell.status())),
        Request::Cleanup => {
            shell.clean_leftover();
            Ok(Response::Ok)
        }
        _ => Err((ErrorKind::Busy, "当前会话不接受该命令".into())),
    }
}

fn create(sid: &str, first: bool) -> io::Result<NamedPipeServer> {
    let sddl = identity::wide(&format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{sid})"));
    // SAFETY: descriptor lives through CreateNamedPipe; the kernel copies it.
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .map_err(io::Error::other)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let result = ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                PIPE_NAME,
                (&attributes as *const SECURITY_ATTRIBUTES)
                    .cast_mut()
                    .cast(),
            );
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        result
    }
}
