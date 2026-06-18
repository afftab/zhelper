use std::fs;
use std::path::Path;

use crate::sysutil;

const ASUS_WMI: &str = "/sys/devices/platform/asus-nb-wmi";
const CPUFREQ: &str = "/sys/devices/system/cpu";
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

// ── CPU Governor ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct CpuGovernor {
    pub current: String,
    pub available: Vec<String>,
}

// ── EPP (Energy Performance Preference) ───────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Epp {
    pub current: String,
    pub available: Vec<String>,
}

// ── CpuManager ────────────────────────────────────────────────────────────────

pub struct CpuManager {
    pub thermal_profile: ThermalProfile,
    pub pending_thermal: ThermalProfile,
    pub governor: CpuGovernor,
    pub pending_governor: String,
    pub epp: Epp,
    pub pending_epp: String,
    pub boost_enabled: bool,
    pub pending_boost: bool,
    pub cpu_temp_c: Option<f32>,
    pub fan_cpu_rpm: Option<u32>,
    pub fan_gpu_rpm: Option<u32>,
    // ASUS PPT power limits (watts)
    pub ppt_apu_sppt: Option<u32>,
    pub pending_apu_sppt: Option<u32>,
    pub ppt_fppt: Option<u32>,
    pub pending_fppt: Option<u32>,
    pub ppt_pl1_spl: Option<u32>,
    pub pending_pl1_spl: Option<u32>,
    pub ppt_pl2_sppt: Option<u32>,
    pub pending_pl2_sppt: Option<u32>,
    // NVIDIA
    pub nv_dynamic_boost: Option<u32>,
    pub pending_nv_boost: Option<u32>,
    pub nv_temp_target: Option<u32>,
    pub pending_nv_temp_target: Option<u32>,
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
            governor: CpuGovernor { current: String::new(), available: vec![] },
            pending_governor: String::new(),
            epp: Epp { current: String::new(), available: vec![] },
            pending_epp: String::new(),
            boost_enabled: false,
            pending_boost: false,
            cpu_temp_c: None,
            fan_cpu_rpm: None,
            fan_gpu_rpm: None,
            ppt_apu_sppt: None,
            pending_apu_sppt: None,
            ppt_fppt: None,
            pending_fppt: None,
            ppt_pl1_spl: None,
            pending_pl1_spl: None,
            ppt_pl2_sppt: None,
            pending_pl2_sppt: None,
            nv_dynamic_boost: None,
            pending_nv_boost: None,
            nv_temp_target: None,
            pending_nv_temp_target: None,
            asus_wmi_available,
            boost_available,
        };
        mgr.refresh();
        mgr.pending_thermal = mgr.thermal_profile;
        mgr.pending_governor = mgr.governor.current.clone();
        mgr.pending_epp = mgr.epp.current.clone();
        mgr.pending_boost = mgr.boost_enabled;
        mgr.pending_apu_sppt = mgr.ppt_apu_sppt;
        mgr.pending_fppt = mgr.ppt_fppt;
        mgr.pending_pl1_spl = mgr.ppt_pl1_spl;
        mgr.pending_pl2_sppt = mgr.ppt_pl2_sppt;
        mgr.pending_nv_boost = mgr.nv_dynamic_boost;
        mgr.pending_nv_temp_target = mgr.nv_temp_target;
        mgr
    }

    pub fn refresh(&mut self) {
        if self.asus_wmi_available {
            self.thermal_profile = read_asus("throttle_thermal_policy")
                .map(|s| ThermalProfile::from_sysfs(&s))
                .unwrap_or(ThermalProfile::Unknown);

            self.ppt_apu_sppt = read_asus_u32("ppt_apu_sppt");
            self.ppt_fppt = read_asus_u32("ppt_fppt");
            self.ppt_pl1_spl = read_asus_u32("ppt_pl1_spl");
            self.ppt_pl2_sppt = read_asus_u32("ppt_pl2_sppt");
            self.nv_dynamic_boost = read_asus_u32("nv_dynamic_boost");
            self.nv_temp_target = read_asus_u32("nv_temp_target");

            self.read_fans();
        }

        self.cpu_temp_c = sysutil::read_cpu_temp();

        if !self.governor.current.is_empty() {
            self.governor.current = read_cpufreq("scaling_governor").unwrap_or_default();
        } else {
            self.governor.available = read_cpufreq("scaling_available_governors")
                .map(|s| s.split_whitespace().map(String::from).collect())
                .unwrap_or_default();
            self.governor.current = read_cpufreq("scaling_governor").unwrap_or_default();
        }

        if self.epp.available.is_empty() {
            self.epp.available = read_cpufreq("energy_performance_available_preferences")
                .map(|s| s.split_whitespace().map(String::from).collect())
                .unwrap_or_default();
        }
        self.epp.current = read_cpufreq("energy_performance_preference").unwrap_or_default();

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

    pub fn apply_governor(&mut self) -> Result<(), String> {
        let cpus = sysutil::count_cpus();
        for i in 0..cpus {
            let path = format!("{CPUFREQ}/cpu{i}/cpufreq/scaling_governor");
            sysutil::write_sysfs(&path, &self.pending_governor)?;
        }
        self.governor.current = self.pending_governor.clone();
        Ok(())
    }

    pub fn apply_epp(&mut self) -> Result<(), String> {
        let cpus = sysutil::count_cpus();
        for i in 0..cpus {
            let path = format!("{CPUFREQ}/cpu{i}/cpufreq/energy_performance_preference");
            sysutil::write_sysfs(&path, &self.pending_epp)?;
        }
        self.epp.current = self.pending_epp.clone();
        Ok(())
    }

    pub fn apply_boost(&mut self) -> Result<(), String> {
        let val = if self.pending_boost { "1" } else { "0" };
        sysutil::write_sysfs(BOOST_PATH, val)?;
        self.boost_enabled = self.pending_boost;
        Ok(())
    }

    pub fn apply_ppt(&mut self, which: PptKind) -> Result<(), String> {
        let (file, value) = match which {
            PptKind::ApuSppt => ("ppt_apu_sppt", self.pending_apu_sppt),
            PptKind::Fppt => ("ppt_fppt", self.pending_fppt),
            PptKind::Pl1Spl => ("ppt_pl1_spl", self.pending_pl1_spl),
            PptKind::Pl2Sppt => ("ppt_pl2_sppt", self.pending_pl2_sppt),
            PptKind::NvBoost => ("nv_dynamic_boost", self.pending_nv_boost),
            PptKind::NvTempTarget => ("nv_temp_target", self.pending_nv_temp_target),
        };
        let v = value.ok_or("No value set")?;
        sysutil::write_asus(file, &v.to_string())?;
        match which {
            PptKind::ApuSppt => self.ppt_apu_sppt = Some(v),
            PptKind::Fppt => self.ppt_fppt = Some(v),
            PptKind::Pl1Spl => self.ppt_pl1_spl = Some(v),
            PptKind::Pl2Sppt => self.ppt_pl2_sppt = Some(v),
            PptKind::NvBoost => self.nv_dynamic_boost = Some(v),
            PptKind::NvTempTarget => self.nv_temp_target = Some(v),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PptKind {
    ApuSppt,
    Fppt,
    Pl1Spl,
    Pl2Sppt,
    NvBoost,
    NvTempTarget,
}

impl PptKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ApuSppt => "CPU Sustained (PPT APU)",
            Self::Fppt => "CPU Fast (PPT FPPT)",
            Self::Pl1Spl => "CPU Long-term (PL1 SPL)",
            Self::Pl2Sppt => "CPU Short-term (PL2 SPPT)",
            Self::NvBoost => "GPU Dynamic Boost",
            Self::NvTempTarget => "GPU Temp Target",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::ApuSppt => "Sustained CPU power limit in watts",
            Self::Fppt => "Short burst CPU power limit in watts",
            Self::Pl1Spl => "CPU long-term power limit (Intel-style)",
            Self::Pl2Sppt => "CPU short-term power limit (Intel-style)",
            Self::NvBoost => "How much power GPU can borrow from CPU (W)",
            Self::NvTempTarget => "GPU thermal throttle target (deg C)",
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_asus(name: &str) -> Option<String> {
    fs::read_to_string(format!("{ASUS_WMI}/{name}"))
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_asus_u32(name: &str) -> Option<u32> {
    read_asus(name).and_then(|s| s.parse().ok())
}

fn read_cpufreq(name: &str) -> Option<String> {
    fs::read_to_string(format!("{CPUFREQ}/cpu0/cpufreq/{name}"))
        .ok()
        .map(|s| s.trim().to_string())
}
