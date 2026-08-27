//! Installing (and inspecting) the nightly compile.
//!
//! Whatever the platform's scheduler is, the contract is the same: run
//! `ragpilot brain compile` once a day at the configured time. Installation is
//! always explicit — nothing here registers a background job behind the user's
//! back.

use std::path::PathBuf;

use anyhow::{Context, Result};
use colored::Colorize;

use super::config::BrainConfig;

pub const UNIT_NAME: &str = "ragpilot-brain-compile";
pub const LAUNCH_LABEL: &str = "dev.ragpilot.brain-compile";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    SystemdUser,
    Launchd,
    Unsupported,
}

pub fn platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Launchd
    } else if cfg!(target_os = "linux") {
        Platform::SystemdUser
    } else {
        Platform::Unsupported
    }
}

/// `HH:MM` split into hour and minute, rejecting anything that is not a time.
pub fn parse_schedule(raw: &str) -> Option<(u32, u32)> {
    let (h, m) = raw.trim().split_once(':')?;
    let (h, m) = (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

// ── unit file rendering (pure, so it can be tested anywhere) ───────────────

pub fn systemd_service(binary: &str) -> String {
    format!(
        "[Unit]\n\
         Description=RagPilot brain compile\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={binary} brain compile\n"
    )
}

pub fn systemd_timer(hour: u32, minute: u32) -> String {
    format!(
        "[Unit]\n\
         Description=RagPilot brain compile (daily)\n\
         \n\
         [Timer]\n\
         OnCalendar=*-*-* {hour:02}:{minute:02}:00\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n"
    )
}

pub fn launchd_plist(binary: &str, hour: u32, minute: u32) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LAUNCH_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
    <string>brain</string>
    <string>compile</string>
  </array>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key><integer>{hour}</integer>
    <key>Minute</key><integer>{minute}</integer>
  </dict>
</dict>
</plist>
"#
    )
}

fn systemd_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("systemd")
        .join("user")
}

fn launch_agents_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library")
        .join("LaunchAgents")
}

/// Where the scheduler definition lives on this platform.
pub fn unit_paths() -> Vec<PathBuf> {
    match platform() {
        Platform::SystemdUser => vec![
            systemd_dir().join(format!("{UNIT_NAME}.service")),
            systemd_dir().join(format!("{UNIT_NAME}.timer")),
        ],
        Platform::Launchd => vec![launch_agents_dir().join(format!("{LAUNCH_LABEL}.plist"))],
        Platform::Unsupported => Vec::new(),
    }
}

/// Whether a scheduler definition is present.
pub fn installed() -> bool {
    let paths = unit_paths();
    !paths.is_empty() && paths.iter().all(|p| p.exists())
}

fn binary_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "ragpilot".to_string())
}

// ── commands ───────────────────────────────────────────────────────────────

pub fn cmd_schedule(args: &[String]) -> Result<()> {
    let action = args.iter().find_map(|a| match a.as_str() {
        "--install" => Some("install"),
        "--remove" => Some("remove"),
        "--print" => Some("print"),
        _ => None,
    });
    match action.unwrap_or("status") {
        "install" => install(),
        "remove" => remove(),
        "print" => print_units(),
        _ => status(),
    }
}

fn schedule_time() -> Result<Option<(u32, u32)>> {
    let cfg = BrainConfig::load(&super::config_path())?;
    if cfg.compiler.schedule.trim().is_empty() {
        return Ok(None);
    }
    parse_schedule(&cfg.compiler.schedule)
        .map(Some)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "compiler.schedule is '{}', which is not an HH:MM time.",
                cfg.compiler.schedule
            )
        })
}

fn print_units() -> Result<()> {
    let Some((h, m)) = schedule_time()? else {
        println!("{} compiler.schedule is empty — nothing to schedule.", "i".blue());
        return Ok(());
    };
    let binary = binary_path();
    for (path, body) in rendered(&binary, h, m) {
        println!("{}\n{}", format!("── {} ──", path.display()).bold(), body);
    }
    Ok(())
}

fn rendered(binary: &str, hour: u32, minute: u32) -> Vec<(PathBuf, String)> {
    match platform() {
        Platform::SystemdUser => {
            let dir = systemd_dir();
            vec![
                (dir.join(format!("{UNIT_NAME}.service")), systemd_service(binary)),
                (dir.join(format!("{UNIT_NAME}.timer")), systemd_timer(hour, minute)),
            ]
        }
        Platform::Launchd => vec![(
            launch_agents_dir().join(format!("{LAUNCH_LABEL}.plist")),
            launchd_plist(binary, hour, minute),
        )],
        Platform::Unsupported => Vec::new(),
    }
}

fn install() -> Result<()> {
    let Some((h, m)) = schedule_time()? else {
        println!("{} compiler.schedule is empty — set it first, or compile manually.", "i".blue());
        return Ok(());
    };
    if platform() == Platform::Unsupported {
        println!(
            "{} No supported scheduler on this platform — run `ragpilot brain compile` manually \
             (or wire it into your own scheduler; `--print` shows the command).",
            "!".yellow()
        );
        return Ok(());
    }

    let binary = binary_path();
    for (path, body) in rendered(&binary, h, m) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, body).with_context(|| format!("Cannot write {}", path.display()))?;
        println!("{} {}", "✓".green(), path.display());
    }

    match platform() {
        Platform::SystemdUser => {
            run(&["systemctl", "--user", "daemon-reload"]);
            if run(&["systemctl", "--user", "enable", "--now", &format!("{UNIT_NAME}.timer")]) {
                println!("{} Timer enabled for {h:02}:{m:02} daily", "✓".green());
            } else {
                println!(
                    "{} Unit files written, but `systemctl --user enable --now {UNIT_NAME}.timer` \
                     failed — run it yourself once systemd is available.",
                    "!".yellow()
                );
            }
        }
        Platform::Launchd => {
            let plist = launch_agents_dir().join(format!("{LAUNCH_LABEL}.plist"));
            if run(&["launchctl", "load", "-w", &plist.to_string_lossy()]) {
                println!("{} Agent loaded for {h:02}:{m:02} daily", "✓".green());
            } else {
                println!("{} Plist written, but `launchctl load` failed.", "!".yellow());
            }
        }
        Platform::Unsupported => {}
    }
    Ok(())
}

fn remove() -> Result<()> {
    match platform() {
        Platform::SystemdUser => {
            run(&["systemctl", "--user", "disable", "--now", &format!("{UNIT_NAME}.timer")]);
        }
        Platform::Launchd => {
            let plist = launch_agents_dir().join(format!("{LAUNCH_LABEL}.plist"));
            run(&["launchctl", "unload", "-w", &plist.to_string_lossy()]);
        }
        Platform::Unsupported => {}
    }
    for path in unit_paths() {
        if path.exists() {
            std::fs::remove_file(&path)?;
            println!("{} removed {}", "✓".green(), path.display());
        }
    }
    Ok(())
}

pub fn status() -> Result<()> {
    println!("{}", "─── brain schedule ──────────────────────────────".bold());
    let configured = BrainConfig::load(&super::config_path())
        .map(|c| c.compiler.schedule)
        .unwrap_or_default();
    println!(
        "  configured: {}",
        if configured.trim().is_empty() { "(manual only)".to_string() } else { configured }
    );
    println!("  platform:   {:?}", platform());
    println!("  installed:  {}", if installed() { "yes" } else { "no" });
    for path in unit_paths() {
        println!("    {} {}", if path.exists() { "✓" } else { "·" }, path.display());
    }
    if !installed() {
        println!("  install with: {}", "ragpilot brain schedule --install".bold());
    }
    Ok(())
}

fn run(cmd: &[&str]) -> bool {
    std::process::Command::new(cmd[0])
        .args(&cmd[1..])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_schedule_accepts_only_real_times() {
        assert_eq!(parse_schedule("18:00"), Some((18, 0)));
        assert_eq!(parse_schedule(" 07:05 "), Some((7, 5)));
        assert_eq!(parse_schedule("23:59"), Some((23, 59)));

        assert_eq!(parse_schedule("24:00"), None);
        assert_eq!(parse_schedule("18:60"), None);
        assert_eq!(parse_schedule("18"), None);
        assert_eq!(parse_schedule(""), None);
        assert_eq!(parse_schedule("evening"), None);
    }

    #[test]
    fn the_systemd_units_run_the_compile_at_the_configured_time() {
        let service = systemd_service("/usr/local/bin/ragpilot");
        assert!(service.contains("ExecStart=/usr/local/bin/ragpilot brain compile"));
        assert!(service.contains("Type=oneshot"));

        let timer = systemd_timer(18, 5);
        assert!(timer.contains("OnCalendar=*-*-* 18:05:00"));
        // A machine that was asleep at 18:05 still compiles when it wakes.
        assert!(timer.contains("Persistent=true"));
        assert!(timer.contains("WantedBy=timers.target"));
    }

    #[test]
    fn the_launchd_plist_is_well_formed() {
        let plist = launchd_plist("/usr/local/bin/ragpilot", 9, 30);
        assert!(plist.contains(LAUNCH_LABEL));
        assert!(plist.contains("<string>brain</string>"));
        assert!(plist.contains("<string>compile</string>"));
        assert!(plist.contains("<key>Hour</key><integer>9</integer>"));
        assert!(plist.contains("<key>Minute</key><integer>30</integer>"));
        assert_eq!(plist.matches("<dict>").count(), plist.matches("</dict>").count());
        assert_eq!(plist.matches("<array>").count(), plist.matches("</array>").count());
    }

    #[test]
    fn unit_paths_match_the_platform() {
        let paths = unit_paths();
        match platform() {
            Platform::SystemdUser => {
                assert_eq!(paths.len(), 2);
                assert!(paths.iter().any(|p| p.to_string_lossy().ends_with(".timer")));
            }
            Platform::Launchd => assert_eq!(paths.len(), 1),
            Platform::Unsupported => assert!(paths.is_empty()),
        }
    }
}
