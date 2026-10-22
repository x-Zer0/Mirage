//! Working copy of the extra-rules editor (iteration 2 §3.3). It lives on
//! the UI thread and only reaches the core as YAML text on "保存".
use super::{Command, MainWindow, RuleRow, send};
use crate::app::OverrideKind;
use crate::overrides::rules;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tokio::sync::mpsc;

thread_local! {
    static ROWS: Rc<VecModel<RuleRow>> = Rc::new(VecModel::default());
    /// YAML the rows came from; returned as is (comments kept) until a visual edit.
    static BASE: RefCell<String> = const { RefCell::new(String::new()) };
    static EDITED: Cell<bool> = const { Cell::new(false) };
}

pub fn wire(window: &MainWindow, commands: &mpsc::Sender<Command>) {
    window.set_rule_rows(ROWS.with(|r| ModelRc::from(r.clone())));
    window.on_rule_move(|index, delta| {
        let (Ok(from), Ok(to)) = (usize::try_from(index), usize::try_from(index + delta)) else {
            return;
        };
        ROWS.with(|rows| {
            if from < rows.row_count() && to < rows.row_count() {
                let row = rows.remove(from);
                rows.insert(to, row);
                EDITED.set(true);
            }
        });
    });
    window.on_rule_delete(|index| {
        ROWS.with(|rows| {
            if let Ok(i) = usize::try_from(index)
                && i < rows.row_count()
            {
                rows.remove(i);
                EDITED.set(true);
            }
        });
    });
    window.on_rule_check(|kind, payload, target, no_resolve| {
        let kind = usize::try_from(kind).unwrap_or(usize::MAX);
        match rules::build(kind, &payload, &target, no_resolve) {
            Ok(_) => SharedString::new(),
            Err(e) => e.into(),
        }
    });
    window.on_rule_submit(|index, kind, payload, target, no_resolve| {
        let kind = usize::try_from(kind).unwrap_or(usize::MAX);
        let rule = match rules::build(kind, &payload, &target, no_resolve) {
            Ok(rule) => rule,
            Err(e) => return e.into(),
        };
        ROWS.with(|rows| match usize::try_from(index) {
            Ok(i) if i < rows.row_count() => rows.set_row_data(i, row(&rule)),
            _ => rows.push(row(&rule)),
        });
        EDITED.set(true);
        SharedString::new()
    });
    window.on_rules_to_yaml(|| current_yaml().into());
    window.on_rules_from_yaml(|text| match rules::parse_yaml(&text) {
        Ok(list) => {
            load(&text, &list);
            SharedString::new()
        }
        Err(errors) => errors.join("\n").into(),
    });
    let w = window.as_weak();
    window.on_rules_reset(move || {
        if let Some(w) = w.upgrade() {
            reset(&w, &w.get_rules_saved_text(), w.get_rules_saved_enabled());
        }
    });
    let c = commands.clone();
    window.on_save_rules(move |enabled, yaml_mode, text| {
        let text = if yaml_mode {
            text.to_string()
        } else {
            current_yaml()
        };
        send(
            &c,
            Command::SaveOverride {
                kind: OverrideKind::Rules,
                enabled,
                text,
            },
        );
    });
}

/// Replaces the working copy with the saved state (startup, after a save, "放弃修改").
pub fn reset(w: &MainWindow, text: &str, enabled: bool) {
    load(text, &rules::parse_yaml(text).unwrap_or_default());
    w.set_rules_yaml(text.into());
    w.set_rules_enabled(enabled);
    w.set_rules_mode_error(SharedString::new());
}

fn load(text: &str, list: &[String]) {
    ROWS.with(|rows| rows.set_vec(list.iter().map(|r| row(r)).collect::<Vec<_>>()));
    BASE.with(|b| *b.borrow_mut() = text.to_owned());
    EDITED.set(false);
}

fn current_yaml() -> String {
    if !EDITED.get() {
        return BASE.with(|b| b.borrow().clone());
    }
    let list: Vec<String> = ROWS.with(|rows| rows.iter().map(|r| r.raw.to_string()).collect());
    rules::to_yaml(&list)
}

fn row(rule: &str) -> RuleRow {
    let Ok(parsed) = rules::validate(rule) else {
        return RuleRow {
            raw: rule.into(),
            kind: SharedString::new(),
            kind_index: -1,
            payload: rule.into(),
            target: SharedString::new(),
            no_resolve: false,
        };
    };
    RuleRow {
        raw: rule.into(),
        kind_index: parsed
            .visual_index()
            .and_then(|i| i32::try_from(i).ok())
            .unwrap_or(-1),
        no_resolve: parsed.no_resolve(),
        kind: parsed.kind.into(),
        payload: parsed.payload.into(),
        target: parsed.target.into(),
    }
}
