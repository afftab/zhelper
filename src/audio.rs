use serde::Deserialize;
use std::process::Command;

// ── serde models for `pactl -f json list` ────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Availability {
    Available,
    #[serde(rename = "not available")]
    NotAvailable,
    #[default]
    #[serde(other)]
    Unknown,
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Availability::Available)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonPort {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub availability: Availability,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonDevice {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub mute: bool,
    /// volume is a map of channel → {value_percent, …}; we read the first channel.
    pub volume: serde_json::Value,
    #[serde(default)]
    pub ports: Vec<JsonPort>,
    pub active_port: Option<String>,
}

// ── public-facing structs used by the TUI ─────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AudioPort {
    pub port_name: String,
    pub display_name: String,
    pub available: bool,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct AudioDevice {
    pub pactl_name: String,
    pub display_name: String,
    pub volume: f32,
    pub muted: bool,
    pub is_default: bool,
    pub ports: Vec<AudioPort>,
    #[allow(dead_code)]
    pub active_port: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AudioManager {
    pub sinks: Vec<AudioDevice>,
    pub sources: Vec<AudioDevice>,
    pub section: usize, // 0 = output, 1 = input
    pub cursor: usize,
}

impl AudioManager {
    pub fn new() -> Self {
        let mut mgr = Self::default();
        mgr.refresh();
        mgr
    }

    pub fn refresh(&mut self) {
        self.sinks = Self::read_pactl_json("sinks", false);
        self.sources = Self::read_pactl_json("sources", true);
        let max = self.total_rows().saturating_sub(1);
        if self.cursor > max {
            self.cursor = 0;
        }
    }

    pub fn current_devices(&self) -> &[AudioDevice] {
        match self.section {
            0 => &self.sinks,
            _ => &self.sources,
        }
    }

    /// Returns (device_index, port_index_opt) for the current cursor position.
    /// Row 0 = device header; rows 1.. = ports.
    pub fn cursor_info(&self) -> (usize, Option<usize>) {
        let devices = self.current_devices();
        let mut row = 0usize;
        for (di, dev) in devices.iter().enumerate() {
            if self.cursor == row {
                return (di, None);
            }
            row += 1;
            for (pi, _) in dev.ports.iter().enumerate() {
                if self.cursor == row {
                    return (di, Some(pi));
                }
                row += 1;
            }
        }
        (0, None)
    }

    pub fn total_rows(&self) -> usize {
        self.current_devices()
            .iter()
            .map(|d| 1 + d.ports.len())
            .sum()
    }

    pub fn toggle_mute(&mut self) -> Result<(), String> {
        let (di, _) = self.cursor_info();
        let dev = self.current_devices().get(di).ok_or("No device")?;
        let cmd = if self.section == 0 { "set-sink-mute" } else { "set-source-mute" };
        Self::run_pactl(cmd, &[&dev.pactl_name, "toggle"])
    }

    pub fn set_volume(&mut self, delta: i32) -> Result<(), String> {
        let (di, _) = self.cursor_info();
        let dev = self.current_devices().get(di).ok_or("No device")?;
        let current_pct = (dev.volume * 100.0).round() as i32;
        let new_pct = (current_pct + delta).clamp(0, 150);
        let vol_str = format!("{new_pct}%");
        let cmd = if self.section == 0 { "set-sink-volume" } else { "set-source-volume" };
        Self::run_pactl(cmd, &[&dev.pactl_name, &vol_str])
    }

    pub fn set_default(&mut self) -> Result<(), String> {
        let (di, _) = self.cursor_info();
        let dev = self.current_devices().get(di).ok_or("No device")?;
        let cmd = if self.section == 0 { "set-default-sink" } else { "set-default-source" };
        Self::run_pactl(cmd, &[&dev.pactl_name])
    }

    /// Activate the port under the cursor.
    pub fn activate_port(&mut self) -> Result<(), String> {
        let (di, pi) = self.cursor_info();
        let pi = pi.ok_or("Select a port, not the device header")?;
        let dev = self.current_devices().get(di).ok_or("No device")?;
        let port = dev.ports.get(pi).ok_or("No port")?;
        let cmd = if self.section == 0 { "set-sink-port" } else { "set-source-port" };
        Self::run_pactl(cmd, &[&dev.pactl_name, &port.port_name])
    }

    // ── internal helpers ──────────────────────────────────────────────────────

    fn run_pactl(sub: &str, args: &[&str]) -> Result<(), String> {
        let out = Command::new("pactl")
            .args([sub])
            .args(args)
            .output()
            .map_err(|e| format!("pactl failed: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(err_msg(&out.stderr))
        }
    }

    /// Query `pactl -f json list <kind>`, deserialize, and filter into AudioDevice.
    fn read_pactl_json(kind: &str, is_source: bool) -> Vec<AudioDevice> {
        let out = Command::new("pactl")
            .args(["-f", "json", "list", kind])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        let json_devices: Vec<JsonDevice> = serde_json::from_str(&out).unwrap_or_default();
        let default_name = get_default_device(is_source);

        json_devices
            .into_iter()
            .filter(|d| {
                // Hide monitor devices from the source list.
                !(is_source && d.name.ends_with(".monitor"))
            })
            .map(|d| {
                let is_default = default_name.as_deref() == Some(d.name.as_str());
                let ports = d
                    .ports
                    .iter()
                    .map(|p| AudioPort {
                        port_name: p.name.clone(),
                        display_name: p.description.clone(),
                        available: p.availability.is_available(),
                        is_active: d.active_port.as_deref() == Some(p.name.as_str()),
                    })
                    .collect();

                AudioDevice {
                    display_name: clean_name(&d.description),
                    pactl_name: d.name,
                    volume: parse_volume_pct(&d.volume),
                    muted: d.mute,
                    is_default,
                    ports,
                    active_port: d.active_port,
                }
            })
            .filter(|d| !d.pactl_name.is_empty())
            .collect()
    }
}

fn err_msg(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr).trim().to_string()
}

/// Extract the first channel's value_percent (e.g. "40%") → 0.40.
fn parse_volume_pct(volume: &serde_json::Value) -> f32 {
    // volume is an object like {"front-left": {"value_percent": "40%", ...}, ...}
    if let Some(obj) = volume.as_object() {
        if let Some(first) = obj.values().next() {
            if let Some(pct_str) = first.get("value_percent").and_then(|v| v.as_str()) {
                let num: String = pct_str.trim().chars().filter(|c| c.is_numeric()).collect();
                if let Ok(pct) = num.parse::<f32>() {
                    return pct / 100.0;
                }
            }
        }
    }
    0.0
}

fn get_default_device(is_source: bool) -> Option<String> {
    let cmd = if is_source { "get-default-source" } else { "get-default-sink" };
    Command::new("pactl")
        .args([cmd])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
}

fn clean_name(name: &str) -> String {
    name.replace("Family 17h/19h HD Audio Controller ", "")
        .replace("Rembrandt Radeon High Definition Audio Controller ", "AMD Audio ")
        .replace("HDA NVidia", "NVIDIA Audio")
        .trim()
        .to_string()
}
