use std::fs;
use std::process::{Command, Stdio};
use std::io::Write;

const ASUS_WMI: &str = "/sys/devices/platform/asus-nb-wmi";

pub fn write_asus(file: &str, value: &str) -> Result<(), String> {
    let path = format!("{ASUS_WMI}/{file}");
    write_sysfs(&path, value)
}

pub fn write_sysfs(path: &str, value: &str) -> Result<(), String> {
    for attempt in 0..3 {
        match fs::write(path, value) {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return write_privileged(path, value);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Other || e.raw_os_error() == Some(5) => {
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

