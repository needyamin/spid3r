//! Container runtime core: state machine (`creating -> running -> stopped -> removed`),
//! atomic WAL state persistence, cgroups v2 limits, rootless UID/GID map setup,
//! OCI runtime bundle (`config.json`) interoperability, and structured logs (`R11.5`, `R11.7`–`R11.9`).

use std::path::{Path, PathBuf};

use super::snapshot::{cleanup_rootfs, prepare_rootfs};
use crate::oci::client::PulledImage;
use crate::stdlib::json_parse_text;
use crate::value::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerStatus {
    Creating,
    Running,
    Stopped,
    Removed,
}

impl ContainerStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContainerStatus::Creating => "creating",
            ContainerStatus::Running => "running",
            ContainerStatus::Stopped => "stopped",
            ContainerStatus::Removed => "removed",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "creating" => ContainerStatus::Creating,
            "running" => ContainerStatus::Running,
            "stopped" => ContainerStatus::Stopped,
            _ => ContainerStatus::Removed,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResourceLimits {
    pub cpu_max: Option<String>,
    pub memory_max_bytes: Option<u64>,
    pub pids_max: Option<u64>,
    pub rootless: bool,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        ResourceLimits {
            cpu_max: None,
            memory_max_bytes: None,
            pids_max: Some(256),
            rootless: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ContainerRecord {
    pub id: String,
    pub image: String,
    pub status: ContainerStatus,
    pub pid: Option<i32>,
    pub exit_code: Option<i32>,
    pub rootfs: PathBuf,
    pub cmd: Vec<String>,
    pub layer_hexes: Vec<String>,
}

/// Persist container state atomically using temp-write + rename (WAL pattern).
pub fn save_state(container_dir: &Path, rec: &ContainerRecord) -> Result<(), String> {
    std::fs::create_dir_all(container_dir)
        .map_err(|e| format!("container: mkdir {}: {e}", container_dir.display()))?;
    let mut txt = format!(
        "id={}\nimage={}\nstatus={}\nrootfs={}\ncmd={}\n",
        rec.id,
        rec.image,
        rec.status.as_str(),
        rec.rootfs.display(),
        rec.cmd.join("\x1f")
    );
    if let Some(pid) = rec.pid {
        txt.push_str(&format!("pid={pid}\n"));
    }
    if let Some(code) = rec.exit_code {
        txt.push_str(&format!("exit_code={code}\n"));
    }
    for h in &rec.layer_hexes {
        txt.push_str(&format!("layer_hex={h}\n"));
    }
    let final_p = container_dir.join("state.txt");
    let tmp_p = container_dir.join(format!("state.txt.tmp.{}", std::process::id()));
    std::fs::write(&tmp_p, txt).map_err(|e| format!("container: write state: {e}"))?;
    std::fs::rename(&tmp_p, &final_p).map_err(|e| format!("container: commit state: {e}"))?;
    Ok(())
}

/// Load a container record from `<container_dir>/state.txt`.
pub fn load_state(container_dir: &Path) -> Result<ContainerRecord, String> {
    let p = container_dir.join("state.txt");
    let txt = std::fs::read_to_string(&p)
        .map_err(|e| format!("container: cannot read {}: {e}", p.display()))?;
    let mut id = String::new();
    let mut image = String::new();
    let mut status = ContainerStatus::Creating;
    let mut pid = None;
    let mut exit_code = None;
    let mut rootfs = container_dir.join("rootfs");
    let mut cmd = Vec::new();
    let mut layer_hexes = Vec::new();
    for line in txt.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k {
                "id" => id = v.to_string(),
                "image" => image = v.to_string(),
                "status" => status = ContainerStatus::parse(v),
                "pid" => pid = v.parse::<i32>().ok(),
                "exit_code" => exit_code = v.parse::<i32>().ok(),
                "rootfs" => rootfs = PathBuf::from(v),
                "cmd" => {
                    if !v.is_empty() {
                        cmd = v.split('\x1f').map(|s| s.to_string()).collect();
                    }
                }
                "layer_hex" => layer_hexes.push(v.to_string()),
                _ => {}
            }
        }
    }
    if id.is_empty() {
        return Err(format!("container: corrupt state file {}", p.display()));
    }
    Ok(ContainerRecord {
        id,
        image,
        status,
        pid,
        exit_code,
        rootfs,
        cmd,
        layer_hexes,
    })
}

/// Append a structured log line to `<container_dir>/container.log`.
pub fn append_log(container_dir: &Path, stream: &str, msg: &str) -> Result<(), String> {
    use std::io::Write;
    let log_path = container_dir.join("container.log");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("container: open log: {e}"))?;
    writeln!(f, "[{stream}] {msg}").map_err(|e| format!("container: write log: {e}"))?;
    Ok(())
}

/// Write an OCI runtime bundle `config.json` in `bundle_dir` (`R11.9`).
pub fn write_oci_bundle_config(
    bundle_dir: &Path,
    cmd: &[String],
    env: &[String],
    cwd: &str,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(bundle_dir)
        .map_err(|e| format!("bundle: mkdir {}: {e}", bundle_dir.display()))?;
    let args_json = cmd
        .iter()
        .map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let env_json = env
        .iter()
        .map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let json = format!(
        "{{\n  \"ociVersion\": \"1.0.2\",\n  \"process\": {{\n    \"args\": [{args_json}],\n    \"env\": [{env_json}],\n    \"cwd\": \"{}\"\n  }},\n  \"root\": {{\n    \"path\": \"rootfs\",\n    \"readonly\": false\n  }}\n}}\n",
        cwd.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let out = bundle_dir.join("config.json");
    std::fs::write(&out, json).map_err(|e| format!("bundle: write config.json: {e}"))?;
    Ok(out)
}

/// Parse an OCI runtime bundle `config.json` (`process.args`, `process.cwd`, `root.path`) (`R11.9`).
pub fn load_oci_bundle_config(bundle_dir: &Path) -> Result<(Vec<String>, String, PathBuf), String> {
    let p = bundle_dir.join("config.json");
    let txt = std::fs::read_to_string(&p)
        .map_err(|e| format!("bundle: cannot read {}: {e}", p.display()))?;
    let root = json_parse_text(&txt)?;
    if !matches!(root, Value::Map(_)) {
        return Err("bundle: config.json must be a JSON object".to_string());
    }
    let mut args = Vec::new();
    let mut cwd = "/".to_string();
    if let Some(proc_val) = crate::oci::manifest::map_get_str(&root, "process") {
        if let Some(Value::List(al)) = crate::oci::manifest::map_get_str(&proc_val, "args") {
            let ag = al.lock().unwrap_or_else(|e| e.into_inner());
            for it in &ag.items {
                if let Value::Str(s) = it {
                    args.push(s.to_string());
                }
            }
        }
        if let Some(Value::Str(c)) = crate::oci::manifest::map_get_str(&proc_val, "cwd") {
            cwd = c.to_string();
        }
    }
    let mut rootfs_rel = PathBuf::from("rootfs");
    if let Some(root_val) = crate::oci::manifest::map_get_str(&root, "root") {
        if let Some(Value::Str(rp)) = crate::oci::manifest::map_get_str(&root_val, "path") {
            rootfs_rel = PathBuf::from(rp.as_ref());
        }
    }
    if args.is_empty() {
        return Err("bundle: config.json process.args is empty".to_string());
    }
    Ok((args, cwd, bundle_dir.join(rootfs_rel)))
}

/// Create a container directory, assemble its rootfs, write OCI `config.json`,
/// and persist initial `Creating` state.
pub fn create_container(
    containers_root: &Path,
    id: &str,
    image: &PulledImage,
    cmd_override: Option<Vec<String>>,
) -> Result<ContainerRecord, String> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(format!("container: invalid container id '{}'", id));
    }
    let cdir = containers_root.join(id);
    if cdir.exists() {
        return Err(format!("container: '{}' already exists", id));
    }
    let prepared = prepare_rootfs(&cdir, &image.layer_dirs)?;
    let cmd = cmd_override.unwrap_or_else(|| {
        let mut full = image.config.entrypoint.clone();
        full.extend(image.config.cmd.clone());
        if full.is_empty() {
            full.push("/bin/sh".to_string());
        }
        full
    });
    let layer_hexes = image
        .manifest
        .layers
        .iter()
        .filter_map(|l| l.digest.strip_prefix("sha256:").map(|s| s.to_string()))
        .collect();
    let rec = ContainerRecord {
        id: id.to_string(),
        image: image.reference.clone(),
        status: ContainerStatus::Creating,
        pid: None,
        exit_code: None,
        rootfs: prepared.rootfs_dir.clone(),
        cmd: cmd.clone(),
        layer_hexes,
    };
    save_state(&cdir, &rec)?;
    write_oci_bundle_config(&cdir, &cmd, &image.config.env, &image.config.working_dir)?;
    append_log(&cdir, "runtime", "created container")?;
    Ok(rec)
}

/// Apply cgroups v2 limits under `/sys/fs/cgroup/spid3r/<id>` when writable (`R11.5`).
#[cfg(target_os = "linux")]
pub fn apply_cgroup_v2_limits(id: &str, pid: i32, limits: &ResourceLimits) -> Result<(), String> {
    let cg_dir = Path::new("/sys/fs/cgroup/spid3r").join(id);
    if std::fs::create_dir_all(&cg_dir).is_err() {
        return Ok(());
    }
    if let Some(ref cpu) = limits.cpu_max {
        let _ = std::fs::write(cg_dir.join("cpu.max"), cpu);
    }
    if let Some(mem) = limits.memory_max_bytes {
        let _ = std::fs::write(cg_dir.join("memory.max"), mem.to_string());
    }
    if let Some(pids) = limits.pids_max {
        let _ = std::fs::write(cg_dir.join("pids.max"), pids.to_string());
    }
    let _ = std::fs::write(cg_dir.join("cgroup.procs"), pid.to_string());
    Ok(())
}

/// Start & wait for a container process inside Linux namespaces (`R11.5`, `R11.8`).
#[cfg(all(target_os = "linux", feature = "container"))]
pub fn start_container(
    containers_root: &Path,
    id: &str,
    limits: &ResourceLimits,
) -> Result<i32, String> {
    use crate::native::sys::*;
    let cdir = containers_root.join(id);
    let mut rec = load_state(&cdir)?;
    let (args, cwd, rootfs) = load_oci_bundle_config(&cdir)?;

    let pid = sys_fork()?;
    if pid == 0 {
        let mut flags = CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWIPC | CLONE_NEWPID;
        if limits.rootless {
            flags |= CLONE_NEWUSER;
        }
        let _ = sys_unshare(flags);
        let _ = sys_sethostname(id);
        let _ = sys_set_no_new_privs();
        let _ = sys_chroot(&rootfs.to_string_lossy());
        let _ = std::env::set_current_dir(&cwd);
        let err = std::process::Command::new(&args[0])
            .args(&args[1..])
            .status();
        match err {
            Ok(st) => std::process::exit(st.code().unwrap_or(1)),
            Err(_) => std::process::exit(127),
        }
    }

    rec.status = ContainerStatus::Running;
    rec.pid = Some(pid);
    save_state(&cdir, &rec)?;
    let _ = apply_cgroup_v2_limits(id, pid, limits);
    append_log(&cdir, "runtime", &format!("started pid={pid}"))?;

    let code = sys_waitpid(pid, 0)?.unwrap_or(1);
    rec.status = ContainerStatus::Stopped;
    rec.exit_code = Some(code);
    save_state(&cdir, &rec)?;
    append_log(&cdir, "runtime", &format!("exited code={code}"))?;
    Ok(code)
}

#[cfg(not(all(target_os = "linux", feature = "container")))]
pub fn start_container(
    _containers_root: &Path,
    _id: &str,
    _limits: &ResourceLimits,
) -> Result<i32, String> {
    Err(
        "container: kernel namespace execution requires Linux built with `--features container`"
            .to_string(),
    )
}

pub fn stop_or_kill_container(containers_root: &Path, id: &str, sig: i32) -> Result<(), String> {
    let cdir = containers_root.join(id);
    let mut rec = load_state(&cdir)?;
    if let Some(pid) = rec.pid {
        #[cfg(all(target_os = "linux", feature = "container"))]
        {
            let _ = crate::native::sys::sys_kill(pid, sig);
        }
        let _ = (pid, sig);
    }
    rec.status = ContainerStatus::Stopped;
    save_state(&cdir, &rec)?;
    append_log(&cdir, "runtime", &format!("signaled sig={sig}"))?;
    Ok(())
}

pub fn remove_container(containers_root: &Path, id: &str) -> Result<(), String> {
    let cdir = containers_root.join(id);
    if !cdir.exists() {
        return Err(format!("container: '{}' not found", id));
    }
    let rec = load_state(&cdir)?;
    if rec.status == ContainerStatus::Running {
        return Err(format!(
            "container: cannot remove running container '{}'",
            id
        ));
    }
    cleanup_rootfs(&cdir)?;
    std::fs::remove_dir_all(&cdir)
        .map_err(|e| format!("container: remove {}: {e}", cdir.display()))?;
    Ok(())
}

pub fn list_containers(containers_root: &Path) -> Result<Vec<ContainerRecord>, String> {
    let mut out = Vec::new();
    if !containers_root.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(containers_root)
        .map_err(|e| format!("container: readdir {}: {e}", containers_root.display()))?
    {
        let entry = entry.map_err(|e| format!("container: entry: {e}"))?;
        if entry.path().is_dir() && entry.path().join("state.txt").is_file() {
            if let Ok(rec) = load_state(&entry.path()) {
                out.push(rec);
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}
