//! Rootfs snapshotter: Linux `overlayfs` mount + cross-platform copy-up
//! assembly and refcounted layer garbage collection (`R11.3`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::oci::layers::apply_layer_to_rootfs;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootfsMode {
    Overlay,
    CopyUp,
}

#[derive(Clone, Debug)]
pub struct PreparedRootfs {
    pub mode: RootfsMode,
    pub rootfs_dir: PathBuf,
    pub upper_dir: PathBuf,
    pub work_dir: PathBuf,
}

/// Assemble a container rootfs from ordered `layer_dirs` (base first, top last).
///
/// On Linux with `--features container`, attempts `overlayfs` first (using
/// `lowerdir=top:...:base,upperdir=...,workdir=...`) and falls back to copy-up
/// rootfs assembly if overlayfs is unavailable (e.g. unprivileged/unsupported FS).
pub fn prepare_rootfs(
    container_dir: &Path,
    layer_dirs: &[PathBuf],
) -> Result<PreparedRootfs, String> {
    let rootfs_dir = container_dir.join("rootfs");
    let upper_dir = container_dir.join("upper");
    let work_dir = container_dir.join("work");
    std::fs::create_dir_all(&rootfs_dir)
        .map_err(|e| format!("snapshot: mkdir {}: {e}", rootfs_dir.display()))?;
    std::fs::create_dir_all(&upper_dir)
        .map_err(|e| format!("snapshot: mkdir {}: {e}", upper_dir.display()))?;
    std::fs::create_dir_all(&work_dir)
        .map_err(|e| format!("snapshot: mkdir {}: {e}", work_dir.display()))?;

    #[cfg(all(target_os = "linux", feature = "container"))]
    if !layer_dirs.is_empty() {
        let lower = layer_dirs
            .iter()
            .rev()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(":");
        let opts = format!(
            "lowerdir={},upperdir={},workdir={}",
            lower,
            upper_dir.display(),
            work_dir.display()
        );
        if crate::native::sys::sys_mount(
            "overlay",
            &rootfs_dir.to_string_lossy(),
            Some("overlay"),
            0,
            Some(&opts),
        )
        .is_ok()
        {
            return Ok(PreparedRootfs {
                mode: RootfsMode::Overlay,
                rootfs_dir,
                upper_dir,
                work_dir,
            });
        }
    }

    for layer in layer_dirs {
        apply_layer_to_rootfs(layer, &rootfs_dir)?;
    }
    Ok(PreparedRootfs {
        mode: RootfsMode::CopyUp,
        rootfs_dir,
        upper_dir,
        work_dir,
    })
}

/// Tear down a prepared rootfs (unmounting overlayfs on Linux if mounted) and
/// remove `upper` / `work` / `rootfs`.
pub fn cleanup_rootfs(container_dir: &Path) -> Result<(), String> {
    let _rootfs_dir = container_dir.join("rootfs");
    #[cfg(all(target_os = "linux", feature = "container"))]
    {
        let _ = crate::native::sys::sys_umount2(
            &_rootfs_dir.to_string_lossy(),
            crate::native::sys::MNT_DETACH,
        );
    }
    for sub in ["rootfs", "upper", "work"] {
        let p = container_dir.join(sub);
        if p.exists() {
            std::fs::remove_dir_all(&p)
                .map_err(|e| format!("snapshot: cleanup {}: {e}", p.display()))?;
        }
    }
    Ok(())
}

/// Garbage-collect cached layers under `store_dir/layers` that are not
/// referenced by any image metadata in `store_dir/images/*.meta` or live container.
pub fn gc_unreferenced_layers(store_dir: &Path, containers_root: &Path) -> Result<usize, String> {
    let mut referenced: HashSet<String> = HashSet::new();
    let images_dir = store_dir.join("images");
    if images_dir.is_dir() {
        for entry in std::fs::read_dir(&images_dir)
            .map_err(|e| format!("gc: readdir {}: {e}", images_dir.display()))?
        {
            let entry = entry.map_err(|e| format!("gc: entry: {e}"))?;
            if let Ok(txt) = std::fs::read_to_string(entry.path()) {
                for line in txt.lines() {
                    if let Some(rest) = line.strip_prefix("layer=") {
                        if let Some((digest, _)) = rest.split_once('|') {
                            if let Some(hex) = digest.strip_prefix("sha256:") {
                                referenced.insert(hex.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    if containers_root.is_dir() {
        for entry in std::fs::read_dir(containers_root)
            .map_err(|e| format!("gc: readdir {}: {e}", containers_root.display()))?
        {
            let entry = entry.map_err(|e| format!("gc: entry: {e}"))?;
            let state_p = entry.path().join("state.txt");
            if let Ok(txt) = std::fs::read_to_string(&state_p) {
                for line in txt.lines() {
                    if let Some(rest) = line.strip_prefix("layer_hex=") {
                        referenced.insert(rest.trim().to_string());
                    }
                }
            }
        }
    }

    let layers_dir = store_dir.join("layers");
    if !layers_dir.is_dir() {
        return Ok(0);
    }
    let mut removed = 0usize;
    for entry in std::fs::read_dir(&layers_dir)
        .map_err(|e| format!("gc: readdir {}: {e}", layers_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("gc: entry: {e}"))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !referenced.contains(&name) && entry.path().is_dir() {
            std::fs::remove_dir_all(entry.path())
                .map_err(|e| format!("gc: remove {}: {e}", entry.path().display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}
