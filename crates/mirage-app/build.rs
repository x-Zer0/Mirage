use embed_manifest::{embed_manifest, new_manifest};
use image::ExtendedColorType;
use image::codecs::ico::{IcoEncoder, IcoFrame};
use resvg::{tiny_skia, usvg};
use std::path::PathBuf;

fn main() {
    slint_build::compile("ui/app.slint").expect("compile Slint UI");

    // Application manifest: asInvoker, per-monitor DPI v2, UTF-8 code page, long paths.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(new_manifest("Mirage")).expect("embed Windows manifest");
        embed_icon();
    }
    println!("cargo:rerun-if-changed=assets/icons/app.svg");
    println!("cargo:rerun-if-changed=build.rs");
}

/// Use the same SVG for the window, taskbar and executable/shortcut icon.
fn embed_icon() {
    let svg = std::fs::read("assets/icons/app.svg").expect("read application icon");
    let tree =
        usvg::Tree::from_data(&svg, &usvg::Options::default()).expect("parse application icon");
    let frames: Vec<_> = [16, 20, 24, 32, 40, 48, 64, 128, 256]
        .into_iter()
        .map(|size| icon_frame(&tree, size))
        .collect();
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("build output directory"));
    let icon = out.join("app.ico");
    let file = std::fs::File::create(&icon).expect("create Windows icon");
    IcoEncoder::new(file)
        .encode_images(&frames)
        .expect("encode Windows icon");
    let resource = out.join("app-icon.rc");
    let icon_path = icon.to_string_lossy().replace('\\', "/");
    std::fs::write(&resource, format!("1 ICON \"{icon_path}\"\n")).expect("write icon resource");
    embed_resource::compile_for(&resource, ["Mirage"], embed_resource::NONE)
        .manifest_required()
        .expect("embed Windows icon");
}

fn icon_frame(tree: &usvg::Tree, size: u32) -> IcoFrame<'static> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("allocate icon pixels");
    let scale = size as f32 / tree.size().width();
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    IcoFrame::as_png(
        &pixmap.take_demultiplied(),
        size,
        size,
        ExtendedColorType::Rgba8,
    )
    .expect("encode icon size")
}
