//! Tray actions, close behavior and window visibility.
use super::{AppTray, Command, MainWindow, SHARED, send, window};
use crate::domain::model::RoutingMode;
use crate::platform::windows::tray_balloon;
use slint::ComponentHandle;
use tokio::sync::mpsc;

pub(super) fn send_mode(commands: &mpsc::Sender<Command>, index: i32) {
    if let Some(mode) = usize::try_from(index)
        .ok()
        .and_then(|i| RoutingMode::ALL.get(i))
    {
        send(commands, Command::SetRoutingMode(*mode));
    }
}

pub(super) fn wire_tray(window: &MainWindow, tray: &AppTray, commands: mpsc::Sender<Command>) {
    let w = window.as_weak();
    let t = tray.as_weak();
    let c = commands.clone();
    tray.on_toggle_window(move || {
        let (Some(w), Some(t)) = (w.upgrade(), t.upgrade()) else {
            return;
        };
        if w.window().is_visible() && !w.window().is_minimized() {
            hide_window(&w, &t, &c);
        } else {
            show_window(&w, &t, &c);
        }
    });

    let c = commands.clone();
    let t = tray.as_weak();
    tray.on_toggle_connection(move || {
        let running = t.upgrade().is_some_and(|t| t.get_running());
        send(
            &c,
            if running {
                Command::Disconnect
            } else {
                Command::Connect
            },
        );
    });

    let c = commands.clone();
    tray.on_set_system_proxy(move |on| send(&c, Command::SetSystemProxy(on)));
    let c = commands.clone();
    tray.on_set_tun(move |on| send(&c, Command::SetTun(on)));
    let c = commands.clone();
    tray.on_set_routing_mode(move |i| send_mode(&c, i));

    tray.on_quit_app(move || quit(&commands));
}

fn quit(commands: &mpsc::Sender<Command>) {
    // The core stops after the event loop returns (main.rs); quitting the
    // loop first keeps the UI responsive while the core shuts down.
    let _ = commands.try_send(Command::Quit);
    let _ = slint::quit_event_loop();
}

/// Closing the window hides it to the tray (§17.2), with a one-time hint;
/// with "close to tray" off it quits instead.
pub(super) fn wire_close(window: &MainWindow, tray: &AppTray, commands: mpsc::Sender<Command>) {
    let t = tray.as_weak();
    window.window().on_close_requested(move || {
        let (to_tray, hint_shown) = SHARED.with_borrow(|s| (s.close_to_tray, s.close_hint_shown));
        if !to_tray {
            quit(&commands);
            return slint::CloseRequestResponse::HideWindow;
        }
        if let Some(t) = t.upgrade() {
            t.set_window_visible(false);
        }
        send(&commands, Command::WindowVisible(false));
        if !hint_shown {
            SHARED.with_borrow_mut(|s| s.close_hint_shown = true);
            tray_balloon::show(
                "Mirage 仍在运行",
                "窗口已最小化到托盘。右键托盘图标可断开或退出，可在设置中改为关闭即退出。",
                false,
            );
            send(&commands, Command::CloseHintShown);
        }
        slint::CloseRequestResponse::HideWindow
    });
}

pub(super) fn show_window(w: &MainWindow, t: &AppTray, commands: &mpsc::Sender<Command>) {
    if let Err(e) = w.show() {
        tracing::warn!(error = %e, "show window failed");
        return;
    }
    w.window().set_minimized(false);
    window::refresh(w);
    t.set_window_visible(true);
    send(commands, Command::WindowVisible(true));
}

fn hide_window(w: &MainWindow, t: &AppTray, commands: &mpsc::Sender<Command>) {
    let _ = w.hide();
    t.set_window_visible(false);
    send(commands, Command::WindowVisible(false));
}
