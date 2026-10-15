use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    slint_build::compile("ui/app.slint").expect("compile Slint UI");

    // Application manifest: asInvoker, per-monitor DPI v2, UTF-8 code page, long paths.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(new_manifest("Mirage")).expect("embed Windows manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
