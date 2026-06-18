use std::fs;
use std::path::Path;

use crate::sysutil;

const ASUS_WMI: &str = "/sys/devices/platform/asus-nb-wmi";
const BOOST_PATH: &str = "/sys/devices/system/cpu/cpufreq/boost";

// ── Thermal profile (ASUS throttle_thermal_policy) ────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThermalProfile {
    Silent,
    Balanced,
    Turbo,
    Unknown,
}

impl ThermalProfile {
    pub fn label(self) -> &'static str {
        match self {
            Self::Silent => "Silent",
            Self::Balanced => "Balanced",
            Self::Turbo => "Turbo",
            Self::Unknown => "Unknown",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Silent => "Quietest fans, lowest power, best battery",
            Self::Balanced => "Adaptive fan curve for everyday use",
            Self::Turbo => "Max fan + max power, use when plugged in",
            Self::Unknown => "",
        }
    }

    pub fn sysfs_value(self) -> &'static str {
        match self {
            Self::Silent => "0",
            Self::Balanced => "1",
            Self::Turbo => "2",
            Self::Unknown => "1",
        }
    }

    pub fn from_sysfs(s: &str) -> Self {
        match s.trim() {
            "0" => Self::Silent,
            "1" => Self::Balanced,
            "2" => Self::Turbo,
            _ => Self::Unknown,
        }
    }

    pub fn variants() -> [Self; 3] {
        [Self::Silent, Self::Balanced, Self::Turbo]
    }

    pub fn index(self) -> usize {
        match self {
            Self::Silent => 0,
            Self::Balanced => 1,
            Self::Turbo => 2,
            Self::Unknown => 1,
        }
    }
}

// ── CpuManager ────────────────────────────────────────────────────────────────

pub struct CpuManager {
    pub thermal_profile: ThermalProfile,
    pub pending_thermal: ThermalProfile,
    pub boost_enabled: bool,
    pub pending_boost: bool,
    pub cpu_temp_c: Option<f32>,
    pub fan_cpu_rpm: Option<u32>,
    pub fan_gpu_rpm: Option<u32>,
    pub asus_wmi_available: bool,
    pub boost_available: bool,
}

impl CpuManager {
    pub fn new() -> Self {
        let asus_wmi_available = Path::new(ASUS_WMI).exists();
        let boost_available = Path::new(BOOST_PATH).exists();
        let mut mgr = Self {
            thermal_profile: ThermalProfile::Unknown,
            pending_thermal: ThermalProfile::Unknown,
            boost_enabled: false,
            pending_boost: false,
            cpu_temp_c: None,
            fan_cpu_rpm: None,
            fan_gpu_rpm: None,
            asus_wmi_available,
            boost_available,
        };
        mgr.refresh(None);
        mgr.pending_thermal = mgr.thermal_profile;
        mgr.pending_boost = mgr.boost_enabled;
        mgr
    }

    pub fn refresh(&mut self, cpu_temp: Option<f32>) {
        if self.asus_wmi_available {
            self.thermal_profile = read_asus("throttle_thermal_policy")
                .map(|s| ThermalProfile::from_sysfs(&s))
                .unwrap_or(ThermalProfile::Unknown);

            self.read_fans();
        }

        self.cpu_temp_c = cpu_temp;

        if self.boost_available {
            self.boost_enabled = fs::read_to_string(BOOST_PATH)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .map(|v| v == 1)
                .unwrap_or(false);
        }
    }

    fn read_fans(&mut self) {
        let hwmon_path = format!("{ASUS_WMI}/hwmon");
        let Ok(entries) = fs::read_dir(&hwmon_path) else { return };
        for entry in entries.flatten() {
            let base = entry.path();
            for (fan_label, fan_file) in [("cpu_fan", "fan1_input"), ("gpu_fan", "fan2_input")] {
                let label = fs::read_to_string(base.join(format!(
                    "{}",
                    fan_file.replace("input", "label")
                ))).ok();
                if label.as_deref().map(|s| s.trim()) == Some(fan_label) {
                    let rpm = fs::read_to_string(base.join(fan_file))
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    if fan_label == "cpu_fan" {
                        self.fan_cpu_rpm = rpm;
                    } else {
                        self.fan_gpu_rpm = rpm;
                    }
                }
            }
        }
    }

    // ── Apply methods ──────────────────────────────────────────────────────────

    pub fn apply_thermal_profile(&mut self) -> Result<(), String> {
        let val = self.pending_thermal.sysfs_value();
        sysutil::write_asus("throttle_thermal_policy", val)?;
        self.thermal_profile = self.pending_thermal;
        Ok(())
    }

    pub fn apply_boost(&mut self) -> Result<(), String> {
        let val = if self.pending_boost { "1" } else { "0" };
        sysutil::write_sysfs(BOOST_PATH, val)?;
        self.boost_enabled = self.pending_boost;
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_asus(name: &str) -> Option<String> {
    fs::read_to_string(format!("{ASUS_WMI}/{name}"))
        .ok()
        .map(|s| s.trim().to_string())
}
