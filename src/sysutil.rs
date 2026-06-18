use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::io::Write;

const ASUS_WMI: &str = "/sys/devices/platform/asus-nb-wmi";

#[allow(dead_code)]
pub fn ac_connected() -> bool {
    let base = Path::new("/sys/class/power_supply");
    let Ok(entries) = fs::read_dir(base) else { return false };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(t) = fs::read_to_string(path.join("type")) {
            if t.trim() == "Mains" {
                if let Ok(v) = fs::read_to_string(path.join("online")) {
                    return v.trim() == "1";
                }
            }
        }
    }
    false
}

pub fn write_asus(file: &str, value: &str) -> Result<(), String> {
    let path = format!("{ASUS_WMI}/{file}");
    write_sysfs(&path, value)
}

pub fn write_sysfs(path: &str, value: &str) -> Result<(), String> {
    // Retry on transient I/O errors (common with dgpu_disable during state transitions)
    for attempt in 0..3 {
        match fs::write(path, value) {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return write_privileged(path, value);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Other || e.raw_os_error() == Some(5) => {
                // EIO -- dGPU transitioning, wait and retry
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    continue;
                }
                return Err(format!("Write error (device may be busy): {e}"));
            }
            Err(e) => return Err(format!("Write error: {e}")),
        }
    }
    unreachable!()
}

pub fn write_privileged(path: &str, value: &str) -> Result<(), String> {
    let mut child = Command::new("pkexec")
        .args(["tee", path])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to launch pkexec: {e}"))?;

    if let Some(ref mut stdin) = child.stdin {
        let _ = stdin.write_all(value.as_bytes());
    }

    let out = child.wait_with_output()
        .map_err(|e| format!("pkexec wait failed: {e}"))?;

    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Permission denied or write failed.\n{}",
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

pub fn read_cpu_temp() -> Option<f32> {
    let base = Path::new("/sys/class/thermal");
    let entries = fs::read_dir(base).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_str()?;
        if !name.starts_with("thermal_zone") { continue; }
        let zone_type = fs::read_to_string(path.join("type")).ok()?;
        let zone_type = zone_type.trim();
        if zone_type.contains("x86_pkg_temp") || zone_type.contains("coretemp")
            || zone_type.contains("k10temp") || zone_type.contains("Tdie") || zone_type.contains("Tctl")
        {
            let temp = fs::read_to_string(path.join("temp")).ok()?;
            return temp.trim().parse::<f32>().ok().map(|t| t / 1000.0);
        }
    }
    None
}

pub fn count_cpus() -> usize {
    fs::read_to_string("/proc/cpuinfo")
        .map(|s| s.lines().filter(|l| l.starts_with("processor")).count())
        .unwrap_or(1)
}
