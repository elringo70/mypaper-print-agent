//! Tauri build glue plus the elevation manifest.
//!
//! The window process reads held jobs from `%SystemRoot%\System32\spool\PRINTERS`,
//! which only Administrators can open, so it must run elevated — same reason as
//! the gpui shell it replaces. The webview is local-assets-only for that exact
//! reason: elevated process, no remote content.

fn main() {
    println!("cargo:rerun-if-changed=mpa-desktop.manifest");

    let attributes = tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("mpa-desktop.manifest")),
    );

    tauri_build::try_build(attributes).expect("tauri build failed");
}
