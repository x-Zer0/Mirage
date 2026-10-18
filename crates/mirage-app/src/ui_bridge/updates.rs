//! Throttled state snapshots, notices and live traffic.
use super::{AppTray, MainWindow, SHARED, Shared, models};
use crate::app::live::LiveSink;
use crate::app::state::{AppStateView, Notice, TrafficSample};
use crate::platform::windows::tray_balloon;
use slint::ComponentHandle;
use std::{cell::RefCell, collections::VecDeque, sync::Arc, time::Duration};
use tokio::sync::watch;

const STATE_THROTTLE: Duration = Duration::from_millis(100);

/// Pushes state snapshots to the UI thread, at most once per throttle window.
pub(super) fn forward_state(
    rt: &tokio::runtime::Handle,
    mut state: watch::Receiver<Arc<AppStateView>>,
    window: &MainWindow,
    tray: &AppTray,
) {
    let window = window.as_weak();
    let tray = tray.as_weak();
    rt.spawn(async move {
        loop {
            let snapshot = state.borrow_and_update().clone();
            let tray = tray.clone();
            let posted = window.upgrade_in_event_loop(move |w| {
                SHARED.with_borrow_mut(|shared| {
                    models::apply(&w, &snapshot, &mut shared.applied);
                    if let Some(t) = tray.upgrade() {
                        models::apply_tray(&t, &snapshot);
                    }
                    show_notices(&w, &snapshot, shared);
                });
            });
            if posted.is_err() || state.changed().await.is_err() {
                break;
            }
            tokio::time::sleep(STATE_THROTTLE).await;
        }
    });
}

/// New toasts, tray balloons for events that matter while hidden, and the
/// close behaviour from settings.
fn show_notices(w: &MainWindow, s: &AppStateView, shared: &mut Shared) {
    shared.close_to_tray = s.settings.close_to_tray;
    shared.close_hint_shown |= s.settings.close_hint_shown;
    let newest = [s.toast.as_ref(), s.error.as_ref()]
        .into_iter()
        .flatten()
        .filter(|n| n.seq > shared.shown_seq)
        .collect::<Vec<&Notice>>();
    for n in newest {
        shared.shown_seq = shared.shown_seq.max(n.seq);
        if !n.error {
            w.invoke_show_toast(n.text.clone().into());
        }
        if n.notify && !w.window().is_visible() {
            tray_balloon::show(
                if n.error { "Mirage 出错" } else { "Mirage" },
                &n.text,
                n.error,
            );
        }
    }
}

/// Receives live data on runtime threads and posts it to the UI thread.
pub(super) struct UiSink {
    pub(super) window: slint::Weak<MainWindow>,
}

impl std::fmt::Debug for UiSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UiSink")
    }
}

thread_local! {
    static TRAFFIC: RefCell<VecDeque<TrafficSample>> = RefCell::new(VecDeque::with_capacity(models::HISTORY));
}
impl LiveSink for UiSink {
    fn traffic(&self, sample: TrafficSample) {
        let _ = self.window.upgrade_in_event_loop(move |w| {
            TRAFFIC.with_borrow_mut(|h| {
                if h.len() == models::HISTORY {
                    h.pop_front();
                }
                h.push_back(sample);
                models::apply_traffic(&w, h);
            });
        });
    }
}
