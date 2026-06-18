use std::fs;
use std::path::Path;
use std::process::Command;

use crate::sysutil;

const DGPU_DISABLE: &str = "/sys/devices/platform/asus-nb-wmi/dgpu_disable";
const GPU_MUX_MODE: &str = "/sys/devices/platform/asus-nb-wmi/gpu_mux_mode";
const PENDING_FILE: &str = "/etc/zhelper/gpu_pending";

// ── GpuMode ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Copy)]
pub enum GpuMode {
    Integrated,
    Hybrid,
    Ultimate,
    Unknown,
}

impl GpuMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Integrated => "Integrated",
            Self::Hybrid     => "Hybrid",
            Self::Ultimate   => "Ultimate",
            Self::Unknown    => "Unknown",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Integrated => "iGPU only, dGPU powered off - best battery life",
            Self::Hybrid     => "iGPU + dGPU both active, on-demand rendering",
            Self::Ultimate   => "dGPU as primary via MUX - best performance",
            Self::Unknown    => "",
        }
    }

    pub fn variants(has_mux: bool) -> &'static [Self] {
        if has_mux {
            &[Self::Integrated, Self::Hybrid, Self::Ultimate]
        } else {
            &[Self::Integrated, Self::Hybrid]
        }
    }

    pub fn index(self, has_mux: bool) -> usize {
        match self {
            Self::Integrated => 0,
            Self::Hybrid     => 1,
            Self::Ultimate   => if has_mux { 2 } else { 1 },
            Self::Unknown    => 0,
        }
    }

    /// Returns (dgpu_disable, gpu_mux_mode) values for this mode.
    fn firmware_values(self) -> (u8, u8) {
        match self {
            Self::Integrated => (1, 1),
            Self::Hybrid     => (0, 1),
            Self::Ultimate   => (0, 0),
            Self::Unknown    => (0, 1),
        }
    }
}

// ── GpuManager ────────────────────────────────────────────────────────────────

pub struct GpuManager {
    pub mode: GpuMode,
    pub pending_mode: GpuMode,
    pub queued_mode: Option<GpuMode>,
    pub has_mux: bool,
    pub asus_wmi_available: bool,
    pub igpu_name: Option<String>,
    pub dgpu_name: Option<String>,
}

impl GpuManager {
    pub fn new() -> Self {
        let asus_wmi_available = Path::new(DGPU_DISABLE).exists();
        let has_mux = Path::new(GPU_MUX_MODE).exists();

        let mode = if asus_wmi_available {
            Self::read_mode(has_mux)
        } else {
            GpuMode::Unknown
        };

        let queued_mode = Self::read_queued_mode(has_mux);

        let (igpu_name, dgpu_name) = read_gpu_names();

        Self {
            pending_mode: queued_mode.unwrap_or(mode),
            mode,
            queued_mode,
            has_mux,
            asus_wmi_available,
            igpu_name,
            dgpu_name,
        }
    }

    pub fn refresh(&mut self) {
        if self.asus_wmi_available {
            self.mode = Self::read_mode(self.has_mux);
        }
        self.queued_mode = Self::read_queued_mode(self.has_mux);
    }

    fn read_mode(has_mux: bool) -> GpuMode {
        let dgpu = fs::read_to_string(DGPU_DISABLE)
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok());

        if has_mux {
            let mux = fs::read_to_string(GPU_MUX_MODE)
                .ok()
                .and_then(|s| s.trim().parse::<u8>().ok());
            match (dgpu, mux) {
                (Some(1), _)      => GpuMode::Integrated,
                (_, Some(0))      => GpuMode::Ultimate,
                (_, _)            => GpuMode::Hybrid,
            }
        } else {
            match dgpu {
                Some(1) => GpuMode::Integrated,
                Some(0) => GpuMode::Hybrid,
                _       => GpuMode::Unknown,
            }
        }
    }

    fn read_queued_mode(has_mux: bool) -> Option<GpuMode> {
        let data = fs::read_to_string(PENDING_FILE).ok()?;
        let parts: Vec<&str> = data.trim().split(',').collect();
        if parts.len() != 2 {
            return None;
        }
        let dgpu: u8 = parts[0].trim().parse().ok()?;
        let mux: u8 = parts[1].trim().parse().ok()?;
        if has_mux {
            match (dgpu, mux) {
                (1, _)      => Some(GpuMode::Integrated),
                (_, 0)      => Some(GpuMode::Ultimate),
                _           => Some(GpuMode::Hybrid),
            }
        } else {
            match dgpu {
                1 => Some(GpuMode::Integrated),
                0 => Some(GpuMode::Hybrid),
                _ => None,
            }
        }
    }

    /// Queue GPU mode change. The actual firmware write happens at shutdown
    /// via the zhelper-gpu-shutdown systemd service.
    pub fn queue_gpu_mode(&mut self) -> Result<(), String> {
        if !self.asus_wmi_available {
            return Err("ASUS WMI not available on this device".to_string());
        }

        let (dgpu, mux) = self.pending_mode.firmware_values();
        let data = format!("{dgpu},{mux}\n");

        // Try direct write to pending file
        match fs::write(PENDING_FILE, &data) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                sysutil::write_privileged(PENDING_FILE, &data)?;
            }
            Err(e) => return Err(format!("Failed to write GPU config: {e}")),
        }

        self.queued_mode = Some(self.pending_mode);
        Ok(())
    }
}

fn read_gpu_names() -> (Option<String>, Option<String>) {
    let out = Command::new("lspci")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut igpu: Option<String> = None;
    let mut dgpu: Option<String> = None;

    for line in out.lines() {
        let lower = line.to_lowercase();
        if !lower.contains("vga") && !lower.contains("3d controller") && !lower.contains("display controller") {
            continue;
        }

        let after_addr = match line.splitn(2, ' ').nth(1) {
            Some(s) => s,
            None => continue,
        };
        let name = match after_addr.splitn(2, ": ").nth(1) {
            Some(s) => s,
            None => continue,
        };
        let name = match name.rfind(" (rev ") {
            Some(pos) => name[..pos].trim(),
            None      => name.trim(),
        };

        let lower_name = name.to_lowercase();
        if lower_name.contains("intel") {
            igpu = Some(shorten_gpu_name(name));
        } else if lower_name.contains("nvidia") {
            dgpu = Some(shorten_gpu_name(name));
        } else if lower_name.contains("amd") || lower_name.contains("advanced micro") {
            if lower_name.contains(" rx ") || lower_name.contains("radeon pro") {
                dgpu = Some(shorten_gpu_name(name));
            } else {
                igpu = Some(shorten_gpu_name(name));
            }
        }
    }

    (igpu, dgpu)
}

fn shorten_gpu_name(name: &str) -> String {
    let name = name
        .replace("Intel Corporation ", "Intel ")
        .replace("NVIDIA Corporation ", "NVIDIA ")
        .replace("Advanced Micro Devices, Inc. [AMD/ATI] ", "AMD ")
        .replace("Advanced Micro Devices, Inc. ", "AMD ");
    if let (Some(start), Some(end)) = (name.find('['), name.rfind(']')) {
        if end > start {
            let vendor = name.split_whitespace().next().unwrap_or("");
            let model  = name[start + 1..end].trim();
            return format!("{vendor} {model}");
        }
    }
    name
}
