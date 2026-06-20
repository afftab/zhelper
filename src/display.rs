use std::process::Command;

const DBUS_DEST: &str = "org.gnome.Mutter.DisplayConfig";
const DBUS_PATH: &str = "/org/gnome/Mutter/DisplayConfig";

#[derive(Debug, Clone)]
pub struct DisplayMode {
    pub mode_id: String,
    pub width: u32,
    pub height: u32,
    pub refresh_rate: f64,
    pub is_current: bool,
    #[allow(dead_code)]
    pub is_preferred: bool,
}

#[derive(Debug, Clone)]
pub struct DisplayMonitor {
    pub connector: String,
    pub display_name: String,
    pub modes: Vec<DisplayMode>,
}

#[derive(Debug, Clone)]
pub struct DisplayManager {
    pub serial: u32,
    pub monitors: Vec<DisplayMonitor>,
    pub current_monitor_idx: usize,
    pub current_scale: f64,
    pub pending_mode_idx: usize,
    pub available: bool,
}

impl Default for DisplayManager {
    fn default() -> Self {
        Self {
            serial: 0,
            monitors: vec![],
            current_monitor_idx: 0,
            current_scale: 1.0,
            pending_mode_idx: 0,
            available: false,
        }
    }
}

impl DisplayManager {
    pub fn new() -> Self {
        let mut mgr = Self::default();
        mgr.refresh();
        mgr
    }

    pub fn refresh(&mut self) {
        let (serial, monitors, scale) = match Self::read_current_state() {
            Some(data) => data,
            None => {
                self.available = false;
                return;
            }
        };

        self.serial = serial;
        self.current_scale = scale;
        self.available = !monitors.is_empty();

        let prev_connector = self
            .monitors
            .get(self.current_monitor_idx)
            .map(|m| m.connector.clone());

        self.monitors = monitors;

        if let Some(conn) = prev_connector {
            if let Some(idx) = self.monitors.iter().position(|m| m.connector == conn) {
                self.current_monitor_idx = idx;
            }
        } else {
            self.current_monitor_idx = 0;
        }

        self.pending_mode_idx = self
            .current_monitor_modes()
            .iter()
            .position(|m| m.is_current)
            .unwrap_or(0);
    }

    pub fn current_monitor(&self) -> Option<&DisplayMonitor> {
        self.monitors.get(self.current_monitor_idx)
    }

    /// Returns all modes for the current monitor, sorted by pixels (desc) then refresh rate (desc).
    pub fn current_monitor_modes(&self) -> Vec<&DisplayMode> {
        let monitor = match self.current_monitor() {
            Some(m) => m,
            None => return vec![],
        };

        let mut modes: Vec<&DisplayMode> = monitor.modes.iter().collect();
        modes.sort_by(|a, b| {
            let pa = a.width as u64 * a.height as u64;
            let pb = b.width as u64 * b.height as u64;
            pb.cmp(&pa).then_with(|| {
                b.refresh_rate.partial_cmp(&a.refresh_rate).unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        modes
    }

    pub fn apply_refresh_rate(&mut self, mode_id: &str) -> Result<(), String> {
        let monitor = self
            .current_monitor()
            .ok_or("No monitor selected")?
            .clone();

        let mode_exists = monitor.modes.iter().any(|m| m.mode_id == mode_id);
        if !mode_exists {
            return Err("Mode not available".to_string());
        }

        let connector = monitor.connector.clone();
        let scale = self.current_scale;

        // Build the gdbus call for ApplyMonitorsConfig
        // Method 1 = layout mode
        // logical_monitors: [(x, y, scale, transform, primary, [(connector, mode_id, {props})])]
        let logical_monitors = format!(
            "[(0, 0, {scale}, uint32 0, true, [('{connector}', '{mode_id}', {{}})])]",
        );

        let serial = self.serial;

        let out = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                DBUS_DEST,
                "--object-path",
                DBUS_PATH,
                "--method",
                &format!("{DBUS_DEST}.ApplyMonitorsConfig"),
                &serial.to_string(),
                "1",
                &logical_monitors,
                "{}",
            ])
            .output()
            .map_err(|e| format!("gdbus failed: {e}"))?;

        if out.status.success() {
            self.refresh();
            Ok(())
        } else {
            Err(format!(
                "Failed to apply refresh rate:\n{}",
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    }

    fn read_current_state() -> Option<(u32, Vec<DisplayMonitor>, f64)> {
        let output = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                DBUS_DEST,
                "--object-path",
                DBUS_PATH,
                "--method",
                &format!("{DBUS_DEST}.GetCurrentState"),
            ])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        Self::parse_current_state(&raw)
    }

    fn parse_current_state(raw: &str) -> Option<(u32, Vec<DisplayMonitor>, f64)> {
        let serial: u32 = raw
            .trim_start_matches("(uint32 ")
            .split(',')
            .next()?
            .trim()
            .parse()
            .ok()?;

        // Find current scale from logical monitors section
        // Pattern: (x, y, scale, uint32 transform, primary, ...
        let scale = Self::extract_current_scale(raw).unwrap_or(1.0);

        // Split into monitor blocks. Each monitor starts with (('connector', ...
        let monitors = Self::parse_monitors(raw);

        if monitors.is_empty() {
            return None;
        }

        Some((serial, monitors, scale))
    }

    fn extract_current_scale(raw: &str) -> Option<f64> {
        // The logical monitors section is after the monitors section.
        // Find pattern: ], [(x, y, SCALE, uint32
        let idx = raw.find("], [(")?;
        let after = &raw[idx..];
        // Skip "], [(" to get into the logical monitors
        let start = after.find("(")? + 1;
        let logical = &after[start..];
        let scale_str = logical.split(',').nth(2)?.trim();
        scale_str.parse::<f64>().ok()
    }

    fn parse_monitors(raw: &str) -> Vec<DisplayMonitor> {
        let mut monitors = vec![];

        // Each monitor block starts with (('connector', 'vendor', 'product', 'serial'), [modes...], {props})
        // We find connector blocks by looking for ((' pattern
        let mut pos = 0;

        while pos < raw.len() {
            // Find next "(('" which marks a monitor start
            let rest = &raw[pos..];
            let monitor_start = match rest.find("(('") {
                Some(idx) => idx,
                None => break,
            };
            let abs_start = pos + monitor_start;

            // Extract connector name (between first two single quotes after "(('")
            let after_parens = &raw[abs_start + 3..];
            let connector = match after_parens.split("', '").next() {
                Some(c) if !c.is_empty() && c.len() < 30 => c.to_string(),
                _ => {
                    pos = abs_start + 3;
                    continue;
                }
            };

            // Find the end of this monitor block by counting paren/bracket depth.
            // A monitor block starts at "(('" (depth 2) and we scan until we're back at depth 0
            // relative to the start. We track (), [], and {} nesting, ignoring quotes.
            let block_end = {
                let scan = &raw[abs_start..];
                let mut depth: i32 = 0;
                let mut in_string = false;
                let mut end_pos = raw.len();
                let scan_bytes = scan.as_bytes();
                let mut i = 0;
                while i < scan_bytes.len() {
                    let c = scan_bytes[i];
                    if in_string {
                        if c == b'\'' { in_string = false; }
                    } else {
                        match c {
                            b'\'' => in_string = true,
                            b'(' | b'[' | b'{' => depth += 1,
                            b')' | b']' | b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    end_pos = abs_start + i + 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    i += 1;
                }
                end_pos
            };
            let block = &raw[abs_start..block_end];

            // Extract display name
            let display_name = block
                .split("'display-name': <'")
                .nth(1)
                .and_then(|s| s.split("'>").next())
                .unwrap_or(&connector)
                .to_string();

            // Parse modes from this block
            let modes = Self::parse_modes(block);
            if !modes.is_empty() {
                monitors.push(DisplayMonitor {
                    connector,
                    display_name,
                    modes,
                });
            }

            pos = block_end;
        }

        monitors
    }

    fn parse_modes(block: &str) -> Vec<DisplayMode> {
        let mut modes = vec![];

        // Mode entries look like: ('2560x1600@165.002', 2560, 1600, 165.00178527832031, scale, [scales], {props})
        // We find each by searching for the pattern "'DIGITSxDIGITS@DIGITS.DIGITS'"
        let mut search_pos = 0;
        while let Some(quote_start) = block[search_pos..].find("'") {
            let abs_start = search_pos + quote_start + 1;
            if abs_start >= block.len() {
                break;
            }
            // Check if this looks like a mode ID (digits x digits @ digits.digits)
            let rest = &block[abs_start..];
            let at_pos = match rest.find("@") {
                Some(p) if p < 20 => p,
                _ => {
                    search_pos = abs_start;
                    continue;
                }
            };
            // Verify it's a resolution pattern (digit x digit before @)
            let before_at = &rest[..at_pos];
            let x_pos = match before_at.find("x") {
                Some(p) => p,
                None => {
                    search_pos = abs_start;
                    continue;
                }
            };
            // Verify both sides are numeric
            let (w_str, h_str) = before_at.split_at(x_pos);
            let h_str = &h_str[1..]; // skip the 'x'
            if !w_str.chars().all(|c| c.is_ascii_digit())
                || !h_str.chars().all(|c| c.is_ascii_digit())
            {
                search_pos = abs_start;
                continue;
            }

            // Find the closing quote of the mode ID
            let after_mode = &rest[at_pos..];
            let close_quote = match after_mode.find("'") {
                Some(p) => at_pos + p,
                None => {
                    search_pos = abs_start;
                    continue;
                }
            };
            let mode_id = rest[..close_quote].to_string();

            // Now parse the numbers after the closing quote: ', W, H, rate, ...'
            let after_id = &rest[close_quote + 1..];
            let nums_str = after_id.trim_start_matches(", ");
            let nums: Vec<&str> = nums_str.split(", ").collect();
            if nums.len() < 4 {
                search_pos = abs_start + close_quote;
                continue;
            }

            let width: u32 = match nums[0].parse() {
                Ok(v) => v,
                Err(_) => {
                    search_pos = abs_start + close_quote;
                    continue;
                }
            };
            let height: u32 = match nums[1].parse() {
                Ok(v) => v,
                Err(_) => {
                    search_pos = abs_start + close_quote;
                    continue;
                }
            };
            let refresh_rate: f64 = match nums[2].parse() {
                Ok(v) => v,
                Err(_) => {
                    search_pos = abs_start + close_quote;
                    continue;
                }
            };

            let check_end = block[abs_start..]
                .find("}), ('")
                .or_else(|| block[abs_start..].find("})], {"))
                .or_else(|| block[abs_start..].find("})])"))
                .map(|i| abs_start + i)
                .unwrap_or(block.len());
            let mode_portion = &block[abs_start..check_end];
            let is_current = mode_portion.contains("'is-current': <true>");
            let is_preferred = mode_portion.contains("'is-preferred': <true>");

            modes.push(DisplayMode {
                mode_id,
                width,
                height,
                refresh_rate,
                is_current,
                is_preferred,
            });

            search_pos = abs_start + close_quote;
        }

        modes
    }
}
