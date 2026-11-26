//! `spid3r neofetch` — compact two-column terminal system & toolchain banner.
//!
//! Pure formatting, layout, and host-snapshot normalization with zero `unsafe`
//! code; OS/hardware collection lives in [`super::host_probes`].

use std::time::Instant;

use super::host_probes::{collect_host_snapshot, enable_vt_console, HostSnapshot};

const LEFT_COL_WIDTH: usize = 29;
const BAR_CELLS: usize = 10;

/// Public CLI entry point for `spid3r neofetch`.
pub fn print_for_cli(no_color: bool) {
    let start = Instant::now();
    let snap = collect_host_snapshot();
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    if !no_color {
        enable_vt_console();
    }
    let use_color = should_use_color(no_color);
    print!("{}", render_neofetch(&snap, elapsed_ms, use_color));
}

/// Determine whether ANSI colors should be emitted on stdout.
pub fn should_use_color(no_color_flag: bool) -> bool {
    if no_color_flag {
        return false;
    }
    if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    if std::env::var("TERM").is_ok_and(|t| t == "dumb") {
        return false;
    }
    if std::env::var("CLICOLOR_FORCE").is_ok_and(|v| !v.is_empty() && v != "0") {
        return true;
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::io::IsTerminal::is_terminal(&std::io::stdout())
    }
}

/// Normalize Windows `ProductName`: Windows 11 builds (`>= 22000`) still report
/// `"Windows 10 ..."` in `HKLM\...\CurrentVersion\ProductName`.
#[allow(dead_code)]
pub(crate) fn normalize_windows_product_name(name: &str, build: u32) -> String {
    let trimmed = name.trim();
    if build >= 22000 && trimmed.starts_with("Windows 10") {
        format!("Windows 11{}", &trimmed["Windows 10".len()..])
    } else if trimmed.is_empty() {
        "Windows".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Filter common OEM placeholder strings and combine manufacturer + model cleanly.
#[allow(dead_code)]
pub(crate) fn clean_host_name(mfg: Option<&str>, model: Option<&str>) -> Option<String> {
    fn clean_token(raw: Option<&str>) -> Option<String> {
        let s = raw?.trim();
        if s.is_empty() {
            return None;
        }
        let lower = s.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "to be filled by o.e.m."
                | "default string"
                | "system manufacturer"
                | "system product name"
                | "baseboard product"
                | "not specified"
                | "none"
                | "unknown"
        ) {
            return None;
        }
        Some(s.to_string())
    }

    let m = clean_token(mfg).map(|v| {
        v.replace(" Technology Co., Ltd.", "")
            .replace(" Co., Ltd.", "")
            .replace(" Computer Inc.", "")
            .trim()
            .to_string()
    });
    let p = clean_token(model);
    match (m, p) {
        (Some(mv), Some(pv)) => {
            if pv
                .to_ascii_lowercase()
                .starts_with(&mv.to_ascii_lowercase())
            {
                Some(pv)
            } else {
                Some(format!("{mv} {pv}"))
            }
        }
        (Some(mv), None) => Some(mv),
        (None, Some(pv)) => Some(pv),
        (None, None) => None,
    }
}

/// Compact noisy trademark suffixes and redundant whitespace in CPU brand strings.
#[allow(dead_code)]
pub(crate) fn clean_cpu_name(raw: &str) -> String {
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s = collapsed
        .replace("(R)", "")
        .replace("(TM)", "")
        .replace("(tm)", "")
        .replace(" CPU", "");
    if let Some((head, _)) = s.split_once(" @ ") {
        s = head.trim().to_string();
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Render a 10-cell progress bar using `━`, `╸`, `─`.
pub(crate) fn render_bar(pct: f64, use_color: bool) -> String {
    let clamped = pct.clamp(0.0, 100.0);
    let (active_str, rest_str) = if clamped >= 99.5 {
        ("━".repeat(BAR_CELLS), String::new())
    } else if clamped <= 0.5 {
        (String::new(), "─".repeat(BAR_CELLS))
    } else {
        let full = ((clamped / 10.0).round() as usize).min(BAR_CELLS - 1);
        let empty = BAR_CELLS - 1 - full;
        (format!("{}╸", "━".repeat(full)), "─".repeat(empty))
    };
    if !use_color {
        return format!("{active_str}{rest_str}");
    }
    let fill_color = if clamped >= 85.0 {
        "\x1b[31m"
    } else if clamped >= 60.0 {
        "\x1b[33m"
    } else {
        "\x1b[36m"
    };
    format!("{fill_color}{active_str}\x1b[90m{rest_str}\x1b[0m")
}

/// Count printable character width ignoring ANSI SGR sequences.
pub(crate) fn visible_width(s: &str) -> usize {
    let mut chars = s.chars().peekable();
    let mut width = 0usize;
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for seq_c in chars.by_ref() {
                if ('@'..='~').contains(&seq_c) {
                    break;
                }
            }
        } else {
            width += 1;
        }
    }
    width
}

pub(crate) fn format_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let mins = (secs % 3600) / 60;
    let rem_s = secs % 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m {rem_s}s")
    } else if mins > 0 {
        format!("{mins}m {rem_s}s")
    } else {
        format!("{rem_s}s")
    }
}

pub(crate) fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (idx, ch) in s.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

pub(crate) fn format_utc_offset(mins: i32) -> String {
    let sign = if mins >= 0 { '+' } else { '-' };
    let abs = mins.abs();
    format!("UTC{}{:02}:{:02}", sign, abs / 60, abs % 60)
}

fn format_gib(bytes: u64) -> String {
    let gib = (bytes as f64) / (1024.0 * 1024.0 * 1024.0);
    if gib >= 10.0 {
        format!("{:.1}", gib)
    } else {
        format!("{:.2}", gib)
    }
}

fn target_short_tag() -> &'static str {
    if cfg!(all(target_arch = "wasm32")) {
        "wasm32"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "win64"
    } else if cfg!(all(target_os = "windows", target_arch = "x86")) {
        "win32"
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        "win-arm64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "lin-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "mac-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "macos64"
    } else {
        std::env::consts::OS
    }
}

fn compiler_short_tag() -> &'static str {
    if cfg!(feature = "native") {
        "LLVM 23"
    } else {
        "Bytecode"
    }
}

fn runtime_short_tag() -> &'static str {
    "Zero-Dep"
}

/// Map standard Windows timezone key names (`TimeZoneKeyName`) to IANA IDs.
#[allow(dead_code)]
pub(crate) fn windows_tz_to_iana(key: &str) -> Option<&'static str> {
    match key {
        "UTC" => Some("Etc/UTC"),
        "GMT Standard Time" => Some("Europe/London"),
        "Greenwich Standard Time" => Some("Atlantic/Reykjavik"),
        "W. Europe Standard Time" => Some("Europe/Berlin"),
        "Central Europe Standard Time" => Some("Europe/Budapest"),
        "Romance Standard Time" => Some("Europe/Paris"),
        "Central European Standard Time" => Some("Europe/Warsaw"),
        "E. Europe Standard Time" => Some("Europe/Chisinau"),
        "GTB Standard Time" => Some("Europe/Athens"),
        "FLE Standard Time" => Some("Europe/Kiev"),
        "Turkey Standard Time" => Some("Europe/Istanbul"),
        "Israel Standard Time" => Some("Asia/Jerusalem"),
        "Egypt Standard Time" => Some("Africa/Cairo"),
        "South Africa Standard Time" => Some("Africa/Johannesburg"),
        "Russian Standard Time" => Some("Europe/Moscow"),
        "Arab Standard Time" => Some("Asia/Riyadh"),
        "Arabian Standard Time" => Some("Asia/Dubai"),
        "Iran Standard Time" => Some("Asia/Tehran"),
        "Pakistan Standard Time" => Some("Asia/Karachi"),
        "India Standard Time" => Some("Asia/Kolkata"),
        "Sri Lanka Standard Time" => Some("Asia/Colombo"),
        "Nepal Standard Time" => Some("Asia/Kathmandu"),
        "Bangladesh Standard Time" => Some("Asia/Dhaka"),
        "Central Asia Standard Time" => Some("Asia/Almaty"),
        "Myanmar Standard Time" => Some("Asia/Yangon"),
        "SE Asia Standard Time" => Some("Asia/Bangkok"),
        "China Standard Time" => Some("Asia/Shanghai"),
        "Singapore Standard Time" => Some("Asia/Singapore"),
        "Taipei Standard Time" => Some("Asia/Taipei"),
        "W. Australia Standard Time" => Some("Australia/Perth"),
        "Korea Standard Time" => Some("Asia/Seoul"),
        "Tokyo Standard Time" => Some("Asia/Tokyo"),
        "AUS Eastern Standard Time" => Some("Australia/Sydney"),
        "New Zealand Standard Time" => Some("Pacific/Auckland"),
        "Hawaiian Standard Time" => Some("Pacific/Honolulu"),
        "Alaskan Standard Time" => Some("America/Anchorage"),
        "Pacific Standard Time" => Some("America/Los_Angeles"),
        "Mountain Standard Time" => Some("America/Denver"),
        "Central Standard Time" => Some("America/Chicago"),
        "Eastern Standard Time" => Some("America/New_York"),
        "Atlantic Standard Time" => Some("America/Halifax"),
        "Newfoundland Standard Time" => Some("America/St_Johns"),
        "E. South America Standard Time" => Some("America/Sao_Paulo"),
        "Argentina Standard Time" => Some("America/Argentina/Buenos_Aires"),
        _ => None,
    }
}

fn c(use_color: bool, code: &str, text: &str) -> String {
    if use_color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn build_left_column(use_color: bool) -> Vec<String> {
    let ver = crate::VERSION;
    let compiler = compiler_short_tag();
    let lang = crate::LANGUAGE_VERSION;
    let target = target_short_tag();
    let runtime = runtime_short_tag();

    let subtitle_plain = format!("v{ver} \u{2022} {compiler}");
    // Center visually under `       ◢ S P I D 3 R ◣` (columns 7..22, midpoint ~14.5).
    let sub_pad = 14usize.saturating_sub(subtitle_plain.chars().count() / 2);
    let subtitle = format!(
        "{}{}",
        " ".repeat(sub_pad),
        c(use_color, "90", &subtitle_plain)
    );

    let box_row = |label: &str, val: &str| -> String {
        let mut v_trunc: String = val.chars().take(9).collect();
        while v_trunc.chars().count() < 9 {
            v_trunc.push(' ');
        }
        if use_color {
            format!(
                "    \x1b[90m│\x1b[0m \x1b[37m{label:<8}\x1b[0m \x1b[1;36m{v_trunc}\x1b[0m \x1b[90m│\x1b[0m"
            )
        } else {
            format!("    │ {label:<8} {v_trunc} │")
        }
    };

    let art = [
        "              │",
        "           ╱  │  ╲",
        "       ╱╲  ╲ ▄█▄ ╱  ╱╲",
        "      ╱  ╲──╲▀█▀╱──╱  ╲",
        "     ╱  ╱──╱▄███▄╲──╲  ╲",
        "       ╱  ╱ █████ ╲  ╲",
        "       ╲  ╲ ▀███▀ ╱  ╱",
    ];
    let mut lines: Vec<String> = art.into_iter().map(|l| c(use_color, "1;36", l)).collect();

    lines.push(String::new());
    lines.push(format!(
        "       {}",
        c(use_color, "1;37", "◢ S P I D 3 R ◣")
    ));
    lines.push(subtitle);
    lines.push(String::new());

    if use_color {
        lines.push("    \x1b[90m┌── \x1b[1;36mTOOLCHAIN\x1b[0;90m ───────┐\x1b[0m".to_string());
    } else {
        lines.push("    ┌── TOOLCHAIN ───────┐".to_string());
    }
    lines.push(box_row("Lang", lang));
    lines.push(box_row("Compiler", compiler));
    lines.push(box_row("Host", "Rust Stg0"));
    lines.push(box_row("Target", target));
    lines.push(box_row("Runtime", runtime));
    lines.push(c(use_color, "90", "    └────────────────────┘"));
    lines.push(String::new());

    if use_color {
        let top = ["31", "33", "32", "36", "34", "35"]
            .iter()
            .map(|k| format!("\x1b[{k}m██\x1b[0m"))
            .collect::<Vec<_>>()
            .join(" ");
        let bot = ["91", "93", "92", "96", "94", "95"]
            .iter()
            .map(|k| format!("\x1b[{k}m▀▀\x1b[0m"))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(format!("       {top}"));
        lines.push(format!("       {bot}"));
    } else {
        lines.push("       ██ ██ ██ ██ ██ ██".to_string());
        lines.push("       ▀▀ ▀▀ ▀▀ ▀▀ ▀▀ ▀▀".to_string());
    }

    lines.push(String::new());
    lines.push(format!("         {}", c(use_color, "1;37", "ANSNEW TECH")));
    lines.push(format!("       {}", c(use_color, "90", "Self-Hosted AOT")));
    lines
}

fn sec_header(title: &str, use_color: bool) -> String {
    if use_color {
        format!("\x1b[1;36m◢ {title} ◣\x1b[0m")
    } else {
        format!("◢ {title} ◣")
    }
}

fn sec_row(key: &str, value: &str, use_color: bool) -> String {
    if use_color {
        format!("\x1b[90m┃\x1b[0m \x1b[1;37m{key:<10}\x1b[0m{value}")
    } else {
        format!("┃ {key:<10}{value}")
    }
}

fn sec_spacer(use_color: bool) -> String {
    c(use_color, "90", "┃")
}

fn build_right_column(snap: &HostSnapshot, elapsed_ms: f64, use_color: bool) -> Vec<String> {
    let mut lines = Vec::new();
    let badge = if snap.elevated {
        if cfg!(windows) {
            Some("[ADMIN]")
        } else {
            Some("[ROOT]")
        }
    } else {
        None
    };

    let title = match badge {
        Some(b) if use_color => format!(
            "\x1b[1;36m{}\x1b[90m@\x1b[1;36m{}\x1b[0m  \x1b[1;33m{b}\x1b[0m",
            snap.username, snap.hostname
        ),
        Some(b) => format!("{}@{}  {b}", snap.username, snap.hostname),
        None if use_color => format!(
            "\x1b[1;36m{}\x1b[90m@\x1b[1;36m{}\x1b[0m",
            snap.username, snap.hostname
        ),
        None => format!("{}@{}", snap.username, snap.hostname),
    };
    lines.push(title);
    lines.push(c(
        use_color,
        "90",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━",
    ));
    lines.push(String::new());

    // SYSTEM
    lines.push(sec_header("SYSTEM", use_color));
    lines.push(sec_row("OS", &snap.os_display, use_color));
    if let Some(host) = &snap.host {
        lines.push(sec_row("Host", host, use_color));
    }
    let kernel_val = match &snap.firmware_boot {
        Some(fw) => format!("{}  \u{2022}  {}", snap.kernel, fw),
        None => snap.kernel.clone(),
    };
    lines.push(sec_row("Kernel", &kernel_val, use_color));

    if let Some(up) = snap.uptime_secs {
        let mut parts = vec![format_uptime(up)];
        if let Some(p) = snap.procs {
            parts.push(format!("{} procs", format_thousands(p as u64)));
        }
        if let Some(t) = snap.threads {
            parts.push(format!("{} threads", format_thousands(t as u64)));
        }
        lines.push(sec_row("Uptime", &parts.join("  \u{2022}  "), use_color));
    }

    let term_dim = match (snap.term_cols, snap.term_rows) {
        (Some(cols), Some(rows)) => format!("{} ({cols}x{rows})", snap.terminal),
        _ => snap.terminal.clone(),
    };
    lines.push(sec_row(
        "Shell",
        &format!("{}  \u{2022}  {term_dim}", snap.shell),
        use_color,
    ));
    if let Some(disp) = &snap.display {
        lines.push(sec_row("Display", disp, use_color));
    }
    lines.push(sec_spacer(use_color));

    // HARDWARE
    lines.push(sec_header("HARDWARE", use_color));
    let mut cpu_meta = Vec::new();
    match (snap.cpu_phys_cores, snap.cpu_log_cores) {
        (Some(p), Some(l)) => cpu_meta.push(format!("{p}C/{l}T")),
        (None, Some(l)) => cpu_meta.push(format!("{l}T")),
        (Some(p), None) => cpu_meta.push(format!("{p}C")),
        (None, None) => {}
    }
    if let Some(mhz) = snap.cpu_mhz {
        let freq = format!("@ {:.2} GHz", (mhz as f64) / 1000.0);
        if cpu_meta.is_empty() {
            cpu_meta.push(freq);
        } else {
            let first = &mut cpu_meta[0];
            first.push(' ');
            first.push_str(&freq);
        }
    }
    let cpu_str = if cpu_meta.is_empty() {
        snap.cpu_brand.clone()
    } else {
        format!("{} ({})", snap.cpu_brand, cpu_meta.join(" "))
    };
    lines.push(sec_row("CPU", &cpu_str, use_color));

    if snap.cpu_load_pct.is_some() || snap.cpu_temp_c.is_some() || snap.cpu_l3_bytes.is_some() {
        let mut load_parts = Vec::new();
        if let Some(pct) = snap.cpu_load_pct {
            load_parts.push(format!("{}  {pct}%", render_bar(pct as f64, use_color)));
        }
        if let Some(temp) = snap.cpu_temp_c {
            load_parts.push(format!("{temp}\u{00b0}C"));
        }
        if let Some(l3) = snap.cpu_l3_bytes {
            let mib = (l3 as f64) / (1024.0 * 1024.0);
            if (mib - mib.round()).abs() < 0.05 {
                load_parts.push(format!("L3 {:.0} MiB", mib.round()));
            } else {
                load_parts.push(format!("L3 {:.1} MiB", mib));
            }
        }
        lines.push(sec_row("Load", &load_parts.join("  \u{2022}  "), use_color));
    }

    if let Some(gpu) = &snap.gpu {
        lines.push(sec_row("GPU", gpu, use_color));
    }

    if let (Some(used), Some(total)) = (snap.mem_used, snap.mem_total) {
        let pct = if total > 0 {
            ((used as f64) * 100.0) / (total as f64)
        } else {
            0.0
        };
        let suffix = snap
            .mem_type
            .as_deref()
            .map(|t| format!(" ({t})"))
            .unwrap_or_default();
        let val = format!(
            "{}  {:.0}%  \u{2022}  {} / {} GiB{suffix}",
            render_bar(pct, use_color),
            pct,
            format_gib(used),
            format_gib(total)
        );
        lines.push(sec_row("Memory", &val, use_color));
    }

    if let (Some(s_used), Some(s_total)) = (snap.swap_used, snap.swap_total) {
        if s_total > 0 {
            let pct = ((s_used as f64) * 100.0) / (s_total as f64);
            let val = format!(
                "{}  {:.0}%  \u{2022}  {} / {} GiB (Page)",
                render_bar(pct, use_color),
                pct,
                format_gib(s_used),
                format_gib(s_total)
            );
            lines.push(sec_row("Swap", &val, use_color));
        }
    }

    if let (Some(d_used), Some(d_total)) = (snap.disk_used, snap.disk_total) {
        if d_total > 0 {
            let pct = ((d_used as f64) * 100.0) / (d_total as f64);
            let media = snap
                .disk_media
                .as_deref()
                .map(|m| format!(" {m}"))
                .unwrap_or_default();
            let key = format!("Disk ({})", snap.disk_label);
            let val = format!(
                "{}  {:.0}%  \u{2022}  {} / {} GiB{media}",
                render_bar(pct, use_color),
                pct,
                format_gib(d_used),
                format_gib(d_total)
            );
            lines.push(sec_row(&key, &val, use_color));
        }
    }
    lines.push(sec_spacer(use_color));

    // POWER & BATTERY
    lines.push(sec_header("POWER & BATTERY", use_color));
    if snap.has_battery {
        if let Some(pct) = snap.battery_pct {
            let status = match (
                snap.battery_charging,
                snap.ac_online,
                snap.battery_secs_left,
            ) {
                (true, _, Some(sec)) if sec > 60 => {
                    format!("Charging (~{}m left)", sec / 60)
                }
                (true, _, _) => "Charging".to_string(),
                (false, Some(true), _) if pct >= 99 => "Full (AC Connected)".to_string(),
                (false, Some(true), _) => "AC Connected".to_string(),
                (false, _, Some(sec)) if sec > 60 => {
                    format!("Discharging (~{}m left)", sec / 60)
                }
                _ => "Discharging".to_string(),
            };
            let val = format!(
                "{}  {pct}%  \u{2022}  {status}",
                render_bar(pct as f64, use_color)
            );
            lines.push(sec_row("Charge", &val, use_color));
        }
        if let (Some(full), Some(design)) = (snap.battery_full_mwh, snap.battery_design_mwh) {
            if design > 0 {
                let health = ((full as f64) * 100.0 / (design as f64)).min(100.0);
                let val = format!(
                    "{}  {:.1}% ({} / {} mWh)",
                    render_bar(health, use_color),
                    health,
                    format_thousands(full as u64),
                    format_thousands(design as u64)
                );
                lines.push(sec_row("Health", &val, use_color));
                let mut cyc_parts = Vec::new();
                if let Some(cyc) = snap.battery_cycles {
                    cyc_parts.push(format!("{cyc} cycles"));
                }
                let wear = (100.0 - health).max(0.0);
                cyc_parts.push(format!("Wear {wear:.1}%"));
                if let Some(mv) = snap.battery_voltage_mv {
                    let v = (mv as f64) / 1000.0;
                    if let Some(chem) = &snap.battery_chemistry {
                        cyc_parts.push(format!("{v:.2}V {chem}"));
                    } else {
                        cyc_parts.push(format!("{v:.2}V"));
                    }
                } else if let Some(chem) = &snap.battery_chemistry {
                    cyc_parts.push(chem.clone());
                }
                lines.push(sec_row(
                    "Cycles",
                    &cyc_parts.join("  \u{2022}  "),
                    use_color,
                ));
            }
        }
    } else {
        lines.push(sec_row("Battery", "No Battery (Desktop / AC)", use_color));
    }

    let mut pwr_parts = Vec::new();
    if let Some(plan) = &snap.power_plan {
        pwr_parts.push(plan.clone());
    }
    match snap.ac_online {
        Some(true) => pwr_parts.push("AC Online".to_string()),
        Some(false) => pwr_parts.push("On Battery".to_string()),
        None => {}
    }
    if let Some(rate_mw) = snap.battery_rate_mw {
        if rate_mw != 0 {
            let watts = (rate_mw as f64) / 1000.0;
            let sign = if watts > 0.0 { "+" } else { "" };
            pwr_parts.push(format!("Draw {sign}{watts:.1}W"));
        }
    }
    if !pwr_parts.is_empty() {
        lines.push(sec_row("Power", &pwr_parts.join("  \u{2022}  "), use_color));
    }
    lines.push(sec_spacer(use_color));

    // NETWORK & ENGINE
    lines.push(sec_header("NETWORK & ENGINE", use_color));
    if let Some(ip) = &snap.local_ip {
        let net_val = match &snap.link_info {
            Some(link) => format!("{ip}  \u{2022}  {link}"),
            None => ip.clone(),
        };
        lines.push(sec_row("Local IP", &net_val, use_color));
    }
    let tz_str = match (&snap.timezone_iana, snap.utc_offset_mins) {
        (Some(tz), Some(off)) => format!("{tz} ({})", format_utc_offset(off)),
        (Some(tz), None) => tz.clone(),
        (None, Some(off)) => format_utc_offset(off),
        (None, None) => "Local".to_string(),
    };
    lines.push(sec_row(
        "Locale",
        &format!("{tz_str}  \u{2022}  {}", snap.locale),
        use_color,
    ));
    lines.push(sec_row(
        "Engine",
        &format!("Native Core Fetch  \u{2022}  {elapsed_ms:.1} ms  \u{2022}  0 Deps"),
        use_color,
    ));

    lines
}

/// Render the two-column `spid3r neofetch` display string.
pub(crate) fn render_neofetch(snap: &HostSnapshot, elapsed_ms: f64, use_color: bool) -> String {
    let left = build_left_column(use_color);
    let right = build_right_column(snap, elapsed_ms, use_color);
    let rows = left.len().max(right.len());

    let mut out = String::from("\n");
    for i in 0..rows {
        let l = left.get(i).map(String::as_str).unwrap_or("");
        let r = right.get(i).map(String::as_str).unwrap_or("");
        if r.is_empty() {
            if !l.is_empty() {
                out.push_str(l.trim_end());
            }
        } else {
            let vw = visible_width(l);
            let pad = LEFT_COL_WIDTH.saturating_sub(vw);
            out.push_str(l);
            out.push_str(&" ".repeat(pad));
            out.push_str(r);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_bar_matches_expected_fractions() {
        assert_eq!(render_bar(0.0, false), "──────────");
        assert_eq!(render_bar(11.0, false), "━╸────────");
        assert_eq!(render_bar(28.0, false), "━━━╸──────");
        assert_eq!(render_bar(45.0, false), "━━━━━╸────");
        assert_eq!(render_bar(52.0, false), "━━━━━╸────");
        assert_eq!(render_bar(84.0, false), "━━━━━━━━╸─");
        assert_eq!(render_bar(91.4, false), "━━━━━━━━━╸");
        assert_eq!(render_bar(100.0, false), "━━━━━━━━━━");
        assert_eq!(render_bar(-5.0, false), "──────────");
        assert_eq!(render_bar(140.0, false), "━━━━━━━━━━");
    }

    #[test]
    fn visible_width_strips_ansi_sequences() {
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width("\x1b[1;36m◢ S P I D 3 R ◣\x1b[0m"), 15);
        assert_eq!(visible_width("\x1b[90m│\x1b[0m a"), 3);
    }

    #[test]
    fn helpers_format_cleanly() {
        assert_eq!(format_uptime(4 * 3600 + 32 * 60 + 18), "4h 32m 18s");
        assert_eq!(format_uptime(90_000), "1d 1h 0m");
        assert_eq!(format_thousands(2412), "2,412");
        assert_eq!(format_thousands(60000), "60,000");
        assert_eq!(format_utc_offset(360), "UTC+06:00");
        assert_eq!(format_utc_offset(-300), "UTC-05:00");
        assert_eq!(
            clean_cpu_name("Intel(R) Core(TM) i7-13700H CPU @ 2.40GHz"),
            "Intel Core i7-13700H"
        );
        assert_eq!(
            normalize_windows_product_name("Windows 10 Pro", 26300),
            "Windows 11 Pro"
        );
        assert_eq!(
            clean_host_name(Some("Gigabyte Technology Co., Ltd."), Some("B860M")),
            Some("Gigabyte B860M".to_string())
        );
        assert_eq!(
            clean_host_name(Some("To Be Filled By O.E.M."), Some("Default string")),
            None
        );
        assert_eq!(
            windows_tz_to_iana("Bangladesh Standard Time"),
            Some("Asia/Dhaka")
        );
    }

    fn sample_laptop_snapshot() -> HostSnapshot {
        HostSnapshot {
            username: "needyamin".into(),
            hostname: "DESKTOP-SPID3R".into(),
            elevated: true,
            os_display: "Windows 11 Pro 24H2 (Build 26100)".into(),
            host: Some("ASUS ROG Zephyrus G16 (GU605MV)".into()),
            kernel: "NT 10.0.26100 (x86_64)".into(),
            firmware_boot: Some("UEFI SecureBoot".into()),
            uptime_secs: Some(4 * 3600 + 32 * 60 + 18),
            procs: Some(184),
            threads: Some(2412),
            shell: "cmd.exe".into(),
            terminal: "Windows Terminal".into(),
            term_cols: Some(120),
            term_rows: Some(36),
            display: Some("2560x1600 @ 240Hz  \u{2022}  125% DPI".into()),
            cpu_brand: "Intel Core i7-13700H".into(),
            cpu_phys_cores: Some(14),
            cpu_log_cores: Some(20),
            cpu_mhz: Some(5000),
            cpu_load_pct: Some(28),
            cpu_temp_c: Some(54),
            cpu_l3_bytes: Some(24 * 1024 * 1024),
            gpu: Some("NVIDIA RTX 4060 Laptop (8 GiB)".into()),
            mem_used: Some(8_420_000_000),
            mem_total: Some(16_000_000_000),
            mem_type: Some("DDR5".into()),
            swap_used: Some(440_000_000),
            swap_total: Some(4_000_000_000),
            disk_label: "C:".into(),
            disk_used: Some(230_400_000_000),
            disk_total: Some(512_000_000_000),
            disk_media: Some("NVMe".into()),
            has_battery: true,
            ac_online: Some(true),
            battery_pct: Some(84),
            battery_charging: true,
            battery_secs_left: Some(42 * 60),
            battery_full_mwh: Some(54_840),
            battery_design_mwh: Some(60_000),
            battery_cycles: Some(118),
            battery_voltage_mv: Some(15_840),
            battery_chemistry: Some("Li-Ion".into()),
            battery_rate_mw: Some(24_600),
            power_plan: Some("Balanced".into()),
            local_ip: Some("192.168.1.42".into()),
            link_info: Some("Wi-Fi (866 Mbps)".into()),
            timezone_iana: Some("Asia/Dhaka".into()),
            utc_offset_mins: Some(360),
            locale: "en-US [UTF-8]".into(),
        }
    }

    #[test]
    fn neofetch_layout_aligns_columns_and_renders_all_sections() {
        let snap = sample_laptop_snapshot();
        for use_color in [false, true] {
            let out = render_neofetch(&snap, 11.4, use_color);
            assert!(out.contains("S P I D 3 R"));
            assert!(out.contains("TOOLCHAIN"));
            assert!(out.contains("SYSTEM"));
            assert!(out.contains("HARDWARE"));
            assert!(out.contains("POWER & BATTERY"));
            assert!(out.contains("NETWORK & ENGINE"));
            assert!(out.contains("11.4 ms"));
            if !use_color {
                assert!(!out.contains("\x1b["));
            }
            for line in out.lines().skip(1) {
                if let Some(pos) = line.find("┃") {
                    assert_eq!(
                        visible_width(&line[..pos]),
                        LEFT_COL_WIDTH,
                        "misaligned right column bar on line: {line:?}"
                    );
                }
                if let Some(pos) = line.find("◢ S") {
                    assert!(
                        !line[..pos].contains("┃"),
                        "left logo line corrupted: {line:?}"
                    );
                } else if let Some(pos) = line.find("◢ ") {
                    assert_eq!(
                        visible_width(&line[..pos]),
                        LEFT_COL_WIDTH,
                        "misaligned section heading on line: {line:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn neofetch_handles_desktop_without_battery() {
        let mut snap = sample_laptop_snapshot();
        snap.has_battery = false;
        snap.battery_pct = None;
        let out = render_neofetch(&snap, 8.2, false);
        assert!(out.contains("No Battery (Desktop / AC)"));
        assert!(!out.contains("Charge"));
    }
}
