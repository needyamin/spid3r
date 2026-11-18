//! Container networking: `spid3r0` bridge IPAM pool, veth pair & port
//! publishing descriptors, and clean teardown (`R11.6`).

use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkAllocation {
    pub bridge: String,
    pub gateway_cidr: String,
    pub container_ip_cidr: String,
    pub veth_host: String,
    pub veth_guest: String,
    pub published_ports: Vec<(u16, u16)>,
}

/// Allocate a deterministic unused IPv4 address in `10.88.0.2..=10.88.254.254`
/// from `<state_dir>/ipam.txt` and return the container network allocation.
pub fn allocate_ip(
    state_dir: &Path,
    container_id: &str,
    published_ports: Vec<(u16, u16)>,
) -> Result<NetworkAllocation, String> {
    std::fs::create_dir_all(state_dir)
        .map_err(|e| format!("net: mkdir {}: {e}", state_dir.display()))?;
    let ipam_file = state_dir.join("ipam.txt");
    let existing = std::fs::read_to_string(&ipam_file).unwrap_or_default();
    let mut used: HashSet<String> = HashSet::new();
    for line in existing.lines() {
        if let Some((_, ip)) = line.split_once('=') {
            used.insert(ip.trim().to_string());
        }
    }
    let mut chosen = None;
    'outer: for third in 0u8..=254 {
        for fourth in 2u8..=254 {
            let cand = format!("10.88.{third}.{fourth}");
            if !used.contains(&cand) {
                chosen = Some(cand);
                break 'outer;
            }
        }
    }
    let ip = chosen.ok_or_else(|| "net: IPAM pool 10.88.0.0/16 exhausted".to_string())?;
    let mut new_txt = existing;
    if !new_txt.is_empty() && !new_txt.ends_with('\n') {
        new_txt.push('\n');
    }
    new_txt.push_str(&format!("{container_id}={ip}\n"));
    let tmp = state_dir.join(format!("ipam.txt.tmp.{}", std::process::id()));
    std::fs::write(&tmp, new_txt).map_err(|e| format!("net: write ipam: {e}"))?;
    std::fs::rename(&tmp, &ipam_file).map_err(|e| format!("net: commit ipam: {e}"))?;

    let short = &container_id[..container_id.len().min(8)];
    Ok(NetworkAllocation {
        bridge: "spid3r0".to_string(),
        gateway_cidr: "10.88.0.1/16".to_string(),
        container_ip_cidr: format!("{ip}/16"),
        veth_host: format!("veth{short}"),
        veth_guest: "eth0".to_string(),
        published_ports,
    })
}

/// Release a container's IP allocation on teardown (`R11.6`).
pub fn release_ip(state_dir: &Path, container_id: &str) -> Result<(), String> {
    let ipam_file = state_dir.join("ipam.txt");
    if !ipam_file.is_file() {
        return Ok(());
    }
    let existing =
        std::fs::read_to_string(&ipam_file).map_err(|e| format!("net: read ipam: {e}"))?;
    let kept: Vec<&str> = existing
        .lines()
        .filter(|l| !l.starts_with(&format!("{container_id}=")))
        .collect();
    let out = if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    };
    let tmp = state_dir.join(format!("ipam.txt.tmp.{}", std::process::id()));
    std::fs::write(&tmp, out).map_err(|e| format!("net: write ipam: {e}"))?;
    std::fs::rename(&tmp, &ipam_file).map_err(|e| format!("net: commit ipam: {e}"))?;
    Ok(())
}
