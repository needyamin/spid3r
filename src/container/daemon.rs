//! Container daemon state & UDS API request dispatch (`R11.7`).

use std::path::Path;

use super::runtime::{list_containers, load_state, ContainerRecord};

/// Recover persisted container state after a daemon restart (`R11.7`).
pub fn recover_daemon_state(containers_root: &Path) -> Result<Vec<ContainerRecord>, String> {
    list_containers(containers_root)
}

/// Format `spid3r container inspect <id>` output.
pub fn inspect_text(containers_root: &Path, id: &str) -> Result<String, String> {
    let rec = load_state(&containers_root.join(id))?;
    Ok(format!(
        "{{\n  \"id\": \"{}\",\n  \"image\": \"{}\",\n  \"status\": \"{}\",\n  \"rootfs\": \"{}\"\n}}",
        rec.id,
        rec.image,
        rec.status.as_str(),
        rec.rootfs.display().to_string().replace('\\', "\\\\")
    ))
}
