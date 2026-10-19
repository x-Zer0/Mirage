//! Windows can discard a software surface while Slint still caches its damage.
use super::MainWindow;
use slint::ComponentHandle;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
use std::time::Duration;

pub(super) fn wire_restore(window: &MainWindow) {
    let weak = window.as_weak();
    window.window().on_winit_window_event(move |_, event| {
        if matches!(
            event,
            WindowEvent::Focused(true) | WindowEvent::Occluded(false)
        ) || matches!(event, WindowEvent::Resized(size) if size.width > 0 && size.height > 0)
        {
            let weak = weak.clone();
            // Run after winit has applied the new size/visibility to Slint.
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(window) = weak.upgrade() {
                    refresh(&window);
                }
            });
        }
        EventResult::Propagate
    });
}

pub(super) fn refresh(window: &MainWindow) {
    window.invoke_refresh_surface();
    window.window().request_redraw();
}

pub(super) fn wire_geometry(
    window: &MainWindow,
    commands: tokio::sync::mpsc::Sender<super::Command>,
) {
    let weak = window.as_weak();
    window.on_geometry_changed(move || {
        let Some(window) = weak.upgrade() else { return };
        let width = window.get_window_width();
        let height = window.get_window_height();
        if width < 700.0 || height < 460.0 || window.window().is_minimized() {
            return;
        }
        super::send(
            &commands,
            super::Command::SetUiGeometry(crate::storage::meta::UiGeometry {
                window_width: width.round() as u32,
                window_height: height.round() as u32,
                sidebar_width: window.get_sidebar_width().round() as u32,
            }),
        );
    });
}
