pub mod apply;
pub mod update;
pub mod verify;

// Release-manifest value types live in `mpa-domain` (leaf crate) so the
// agent core can stage a pending update without a dependency cycle.
pub use apply::{
    fetch_verified, launch_installer_and_exit, replace_running_exe, stage_asset, ApplyError,
};
pub use mpa_domain::{ManifestError, ReleaseManifest, UpdateStatus};
pub use update::{check_for_update, UpdateError};
pub use verify::{verify_asset, VerifyError};
