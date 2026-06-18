use std::fs;
use std::path::Path;
use std::process::Command;

use crate::sysutil;

const DGPU_DISABLE: &str = "/sys/devices/platform/asus-nb-wmi/dgpu_disable";

// ── GpuMode ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Copy)]
pub enum GpuMode {
    Eco,
    Standard,
    Unknown,
}

impl GpuMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Eco      => "Eco",
            Self::Standard => "Standard",
            Self::Unknown  => "Unknown",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Eco      => "dGPU off, iGPU only  -  best battery life",
            Self::Standard => "iGPU + dGPU both active  -  on-demand rendering",
            Self::Unknown  => "",
        }
    }

    pub fn variants() -> [Self; 2] {
        [Self::Eco, Self::Standard]
    }

    pub fn index(self) -> usize {
        match self {
            Self::Eco      => 0,
            Self::Standard => 1,
            Self::Unknown  => 0,
        }
    }
}

// ── GpuManager ────────────────────────────────────────────────────────────────

pub struct GpuManager {
    pub mode: GpuMode,
    pub pending_mode: GpuMode,
    pub asus_wmi_available: bool,
    pub igpu_name: Option<String>,
    pub dgpu_name: Option<String>,
}

impl GpuManager {
    pub fn new() -> Self {
        let asus_wmi_available = Path::new(DGPU_DISABLE).exists();

        let mode = if asus_wmi_available {
            Self::read_mode()
        } else {
            GpuMode::Unknown
        };

        let (igpu_name, dgpu_name) = read_gpu_names();

        Self {
            pending_mode: mode,
            mode,
            asus_wmi_available,
            igpu_name,
            dgpu_name,
        }
    }

    pub fn refresh(&mut self) {
        if self.asus_wmi_available {
            self.mode = Self::read_mode();
        }
    }

    fn read_mode() -> GpuMode {
        let val = fs::read_to_string(DGPU_DISABLE)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());

        match val {
            Some(1) => GpuMode::Eco,
            Some(0) => GpuMode::Standard,
            _ => GpuMode::Unknown,
        }
    }

    /// Apply GPU mode via ASUS WMI dgpu_disable.
    pub fn apply_gpu_mode(&mut self) -> Result<(), String> {
        if !self.asus_wmi_available {
            return Err("ASUS WMI dgpu_disable not available on this device".to_string());
        }

        let disable = match self.pending_mode {
            GpuMode::Eco      => 1,
            GpuMode::Standard => 0,
            GpuMode::Unknown  => return Err("Unknown GPU mode".to_string()),
        };

        sysutil::write_asus("dgpu_disable", &disable.to_string())?;
        self.mode = self.pending_mode;
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
