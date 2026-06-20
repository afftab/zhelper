use std::process::Command;

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

#[derive(Debug, Clone)]
pub struct AudioManager {
    pub sinks: Vec<AudioDevice>,
    pub sources: Vec<AudioDevice>,
    pub section: usize,   // 0 = output, 1 = input
    pub cursor: usize,    // which row (device header + ports) is selected
}

impl Default for AudioManager {
    fn default() -> Self {
        Self { sinks: vec![], sources: vec![], section: 0, cursor: 0 }
    }
}

impl AudioManager {
    pub fn new() -> Self {
        let mut mgr = Self::default();
        mgr.refresh();
        mgr
    }

    pub fn refresh(&mut self) {
        self.sinks = Self::read_pactl("sinks");
        self.sources = Self::read_pactl("sources");
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

    /// Returns (total_rows, device_index, port_index_opt) for current cursor.
    /// Row 0 = device header, rows 1.. = ports.
    pub fn cursor_info(&self) -> (usize, Option<usize>) {
        let devices = self.current_devices();
        let mut row = 0usize;
        for (di, dev) in devices.iter().enumerate() {
            if self.cursor == row {
                return (di, None); // on device header
            }
            row += 1;
            for (pi, _port) in dev.ports.iter().enumerate() {
                if self.cursor == row {
                    return (di, Some(pi));
                }
                row += 1;
            }
        }
        (0, None)
    }

    pub fn total_rows(&self) -> usize {
        let devices = self.current_devices();
        devices.iter().map(|d| 1 + d.ports.len()).sum()
    }

    fn read_pactl(kind: &str) -> Vec<AudioDevice> {
        let out = Command::new("pactl")
            .args(["list", kind])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        parse_pactl_devices(&out, kind == "sources")
    }

    pub fn toggle_mute(&mut self) -> Result<(), String> {
        let (di, _) = self.cursor_info();
        let dev = self.current_devices().get(di).ok_or("No device")?;
        let cmd = if self.section == 0 { "set-sink-mute" } else { "set-source-mute" };
        let out = Command::new("pactl")
            .args([cmd, &dev.pactl_name, "toggle"])
            .output()
            .map_err(|e| format!("pactl failed: {e}"))?;
        if out.status.success() { Ok(()) } else { Err(err_msg(&out.stderr)) }
    }

    pub fn set_volume(&mut self, delta: i32) -> Result<(), String> {
        let (di, _) = self.cursor_info();
        let dev = self.current_devices().get(di).ok_or("No device")?;
        let current_pct = (dev.volume * 100.0).round() as i32;
        let new_pct = (current_pct + delta).clamp(0, 150);
        let vol_str = format!("{new_pct}%");
        let cmd = if self.section == 0 { "set-sink-volume" } else { "set-source-volume" };
        let out = Command::new("pactl")
            .args([cmd, &dev.pactl_name, &vol_str])
            .output()
            .map_err(|e| format!("pactl failed: {e}"))?;
        if out.status.success() { Ok(()) } else { Err(err_msg(&out.stderr)) }
    }

    pub fn set_default(&mut self) -> Result<(), String> {
        let (di, _) = self.cursor_info();
        let dev = self.current_devices().get(di).ok_or("No device")?;
        let cmd = if self.section == 0 { "set-default-sink" } else { "set-default-source" };
        let out = Command::new("pactl")
            .args([cmd, &dev.pactl_name])
            .output()
            .map_err(|e| format!("pactl failed: {e}"))?;
        if out.status.success() { Ok(()) } else { Err(err_msg(&out.stderr)) }
    }

    /// Activate the port under cursor.
    pub fn activate_port(&mut self) -> Result<(), String> {
        let (di, pi) = self.cursor_info();
        let pi = pi.ok_or("Select a port, not the device header")?;
        let dev = self.current_devices().get(di).ok_or("No device")?;
        let port = dev.ports.get(pi).ok_or("No port")?;
        let cmd = if self.section == 0 { "set-sink-port" } else { "set-source-port" };
        let out = Command::new("pactl")
            .args([cmd, &dev.pactl_name, &port.port_name])
            .output()
            .map_err(|e| format!("pactl failed: {e}"))?;
        if out.status.success() { Ok(()) } else { Err(err_msg(&out.stderr)) }
    }
}

fn err_msg(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr).trim().to_string()
}

fn parse_pactl_devices(raw: &str, is_source: bool) -> Vec<AudioDevice> {
    let block_prefix = if is_source { "Source #" } else { "Sink #" };
    let default_name = get_default_device(is_source);
    let mut devices = vec![];

    for block in raw.split(block_prefix).skip(1) {
        let mut name = String::new();
        let mut description = String::new();
        let mut volume = 0.0_f32;
        let mut muted = false;
        let mut ports: Vec<AudioPort> = vec![];
        let mut active_port: Option<String> = None;
        let mut in_ports = false;

        for line in block.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with("Name:") {
                name = trimmed[5..].trim().to_string();
                in_ports = false;
            } else if trimmed.starts_with("Description:") {
                description = trimmed[12..].trim().to_string();
            } else if trimmed.starts_with("Mute:") {
                muted = trimmed[5..].trim() == "yes";
            } else if trimmed.starts_with("Volume:") {
                if let Some(pct_pos) = trimmed.find('/') {
                    let after_slash = &trimmed[pct_pos + 1..];
                    let pct_str: String = after_slash.trim().chars()
                        .take_while(|c| c.is_numeric())
                        .collect();
                    if let Ok(pct) = pct_str.parse::<f32>() {
                        volume = pct / 100.0;
                    }
                }
            } else if trimmed == "Ports:" {
                in_ports = true;
            } else if trimmed.starts_with("Active Port:") {
                active_port = Some(trimmed[12..].trim().to_string());
                in_ports = false;
            } else if in_ports && trimmed.contains(": ") && !trimmed.starts_with("Part of") && !trimmed.starts_with("Properties") {
                if let Some(colon_pos) = trimmed.find(": ") {
                    let port_name = trimmed[..colon_pos].trim().to_string();
                    let rest = &trimmed[colon_pos + 2..];
                    let display_name = if let Some(p) = rest.find(" (type:") {
                        rest[..p].trim().to_string()
                    } else {
                        rest.trim().to_string()
                    };
                    let available = rest.contains("available") && !rest.contains("not available");
                    ports.push(AudioPort {
                        port_name,
                        display_name,
                        available,
                        is_active: false,
                    });
                }
            } else if in_ports && (trimmed.is_empty() || !trimmed.starts_with(' ')) {
                in_ports = false;
            }
        }

        if let Some(ref active) = active_port {
            for p in &mut ports {
                p.is_active = p.port_name == *active;
            }
        }

        if is_source && name.ends_with(".monitor") {
            continue;
        }

        let is_default = default_name.as_deref() == Some(name.as_str());

        if !name.is_empty() {
            devices.push(AudioDevice {
                display_name: clean_name(&description),
                pactl_name: name,
                volume,
                muted,
                is_default,
                ports,
                active_port,
            });
        }
    }

    devices
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
