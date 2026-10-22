//! Working selection of the UWP loopback dialog (iteration 2 §6.2), kept on
//! the UI thread; the core only receives the final states on "保存".
use super::{Command, LoopbackRow, MainWindow, send};
use crate::platform::windows::loopback::AppContainer;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::mpsc;

thread_local! {
    static ALL: RefCell<Vec<LoopbackRow>> = const { RefCell::new(Vec::new()) };
    static SHOWN: Rc<VecModel<LoopbackRow>> = Rc::new(VecModel::default());
    static FILTER: RefCell<String> = const { RefCell::new(String::new()) };
}

pub fn wire(window: &MainWindow, commands: &mpsc::Sender<Command>) {
    window.set_loopback_rows(SHOWN.with(|r| ModelRc::from(r.clone())));
    let c = commands.clone();
    window.on_open_loopback(move || send(&c, Command::ListLoopback));
    let w = window.as_weak();
    window.on_loopback_filter(move |text| {
        FILTER.with(|f| *f.borrow_mut() = text.trim().to_lowercase());
        if let Some(w) = w.upgrade() {
            refresh(&w);
        }
    });
    let w = window.as_weak();
    window.on_loopback_toggle(move |sid, checked| {
        ALL.with(|all| {
            if let Some(row) = all.borrow_mut().iter_mut().find(|r| r.sid == sid) {
                row.checked = checked;
            }
        });
        // In place, so the list keeps its scroll position.
        SHOWN.with(|rows| {
            if let Some(i) = rows.iter().position(|r| r.sid == sid)
                && let Some(mut row) = rows.row_data(i)
            {
                row.checked = checked;
                rows.set_row_data(i, row);
            }
        });
        if let Some(w) = w.upgrade() {
            summarize(&w);
        }
    });
    let w = window.as_weak();
    window.on_loopback_check_shown(move |checked| {
        let filter = FILTER.with(|f| f.borrow().clone());
        ALL.with(|all| {
            for row in all.borrow_mut().iter_mut().filter(|r| matches(r, &filter)) {
                row.checked = checked;
            }
        });
        if let Some(w) = w.upgrade() {
            refresh(&w);
        }
    });
    let c = commands.clone();
    window.on_save_loopback(move || {
        let states = ALL.with(|all| {
            all.borrow()
                .iter()
                .map(|r| (r.sid.to_string(), r.checked))
                .collect()
        });
        send(&c, Command::SaveLoopback(states));
    });
}

/// Replaces the selection with what Windows reports.
pub fn reset(w: &MainWindow, apps: &[AppContainer]) {
    ALL.with(|all| {
        *all.borrow_mut() = apps
            .iter()
            .map(|a| LoopbackRow {
                sid: a.sid.as_str().into(),
                name: a.name.as_str().into(),
                display: a.display_name.as_str().into(),
                checked: a.exempt,
            })
            .collect();
    });
    refresh(w);
}

fn refresh(w: &MainWindow) {
    let filter = FILTER.with(|f| f.borrow().clone());
    let shown = ALL.with(|all| {
        all.borrow()
            .iter()
            .filter(|r| matches(r, &filter))
            .cloned()
            .collect::<Vec<_>>()
    });
    SHOWN.with(|rows| rows.set_vec(shown));
    summarize(w);
}

fn summarize(w: &MainWindow) {
    let (checked, total) = ALL.with(|all| {
        let all = all.borrow();
        (all.iter().filter(|r| r.checked).count(), all.len())
    });
    w.set_loopback_summary(SharedString::from(format!("已勾选 {checked} / {total}")));
}

fn matches(row: &LoopbackRow, filter: &str) -> bool {
    filter.is_empty()
        || row.display.to_lowercase().contains(filter)
        || row.name.to_lowercase().contains(filter)
}
