//! Embeds `mpa-ui.manifest` into `mpa-ui.exe` (Windows only).
//!
//! The manifest is what asks Windows to elevate the process: the agent reads
//! the held job's spool file from `%SystemRoot%\System32\spool\PRINTERS`,
//! which needs Administrators. Without it every analysis fails with
//! "Acceso denegado. (os error 5)".

fn main() {
    println!("cargo:rerun-if-changed=mpa-ui.rc");
    println!("cargo:rerun-if-changed=mpa-ui.manifest");

    // Target-based check (not `#[cfg(windows)]`, which describes the host).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("mpa-ui.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
