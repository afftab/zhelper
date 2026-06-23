// ── ZHelper — TUI
// Layout inspired by spotify-tui: sidebar + content + status bar.
// Single phosphor-green accent, near-black ground, minimal chrome.

use std::collections::VecDeque;
use std::io::stdout;
use std::time::{Duration, Instant};

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Sparkline},
    Frame, Terminal,
};

use crate::{
    audio::AudioManager,
    battery::{BatteryManager, ChargeStatus},
    config::Config,
    cpu::{CpuManager, ThermalProfile},
    display::DisplayManager,
    gpu::{GpuManager, GpuMode},
    system::SystemInfo,
};

// ── Palette ───────────────────────────────────────────────────────────────────

const GREEN:  Color = Color::Rgb(61, 220, 132);
const TEXT:   Color = Color::Rgb(255, 255, 255);   // pure white for primary text
const DIM:    Color = Color::Rgb(170, 170, 170);   // light gray — clearly readable
const FAINT:  Color = Color::Rgb(110, 110, 110);   // medium gray — visible but secondary
const DANGER: Color = Color::Rgb(220, 70, 70);
const LINE:   Color = Color::Rgb(50, 50, 50);      // slightly brighter borders

fn g()   -> Style { Style::default().fg(GREEN) }
fn txt() -> Style { Style::default().fg(TEXT) }
fn dim() -> Style { Style::default().fg(DIM) }
fn fnt() -> Style { Style::default().fg(FAINT) }
fn dng() -> Style { Style::default().fg(DANGER) }
fn bg()  -> Style { Style::default().fg(GREEN).add_modifier(Modifier::BOLD) }
fn line_border() -> Style { Style::default().fg(LINE) }

// ── History ring buffer for sparkline graphs ──────────────────────────────────

const HISTORY_LEN: usize = 60;

#[derive(Clone, Default)]
pub(crate) struct History {
    buf: VecDeque<u64>,
    cached: Vec<u64>,
}

impl History {
    fn push(&mut self, v: u64) {
        if self.buf.len() >= HISTORY_LEN {
            self.buf.pop_front();
        }
        self.buf.push_back(v);
        self.cached = self.buf.iter().copied().collect();
    }
    fn data(&self) -> &Vec<u64> {
        &self.cached
    }
}

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
pub enum Focus { Sidebar, Content }

#[derive(Clone)]
pub enum Status { None, Ok(String), Err(String) }

pub struct TuiApp {
    pub battery:          BatteryManager,
    pub system:           SystemInfo,
    pub config:           Config,
    pub gpu:              GpuManager,
    pub cpu:              CpuManager,
    pub audio:            AudioManager,
    pub display:          DisplayManager,
    pub active_tab:       usize,   // 0 battery  1 system  2 cpu  3 gpu  4 audio  5 display  6 settings
    pub focus:            Focus,
    pub desired_limit:    u8,
    pub status:           Status,
    pub status_until:     Option<Instant>,
    pub last_refresh:     Instant,
    pub should_quit:      bool,
    pub settings_cursor:  usize,
    pub cpu_section:      usize,  // which row in cpu tab is focused
    // Overview sparkline history
    pub hist_cpu_temp:    History,
    pub hist_gpu_temp:    History,
    pub hist_mem_pct:     History,
    pub hist_bat_pct:     History,
    pub hist_fan_cpu:     History,
    pub hist_fan_gpu:     History,
    pub hist_power:       History,
    pub needs_redraw:     bool,
}

impl TuiApp {
    fn new() -> Self {
        let config  = Config::load();
        let battery = BatteryManager::new();
        let desired = config.charge_limit;
        let mut app = Self {
            desired_limit: desired,
            battery,
            system: SystemInfo::new(),
            gpu: GpuManager::new(),
            cpu: CpuManager::new(),
            audio: AudioManager::new(),
            display: DisplayManager::new(),
            config,
            active_tab: 0,
            focus: Focus::Sidebar,
            status: Status::None,
            status_until: Option::None,
            last_refresh: Instant::now(),
            should_quit: false,
            settings_cursor: 0,
            cpu_section: 0,
            hist_cpu_temp: History::default(),
            hist_gpu_temp: History::default(),
            hist_mem_pct: History::default(),
            hist_bat_pct: History::default(),
            hist_fan_cpu: History::default(),
            hist_fan_gpu: History::default(),
            hist_power: History::default(),
            needs_redraw: true,
        };
        if app.config.auto_apply_on_start {
            let _ = app.battery.set_charge_limit(app.desired_limit);
        }
        if let Some(boost) = app.config.boost_enabled {
            if app.cpu.boost_available {
                app.cpu.pending_boost = boost;
                let _ = app.cpu.apply_boost();
            }
        }
        app
    }

    fn refresh(&mut self) {
        self.battery.refresh();
        self.system.refresh();
        self.gpu.refresh();
        self.cpu.refresh(self.system.cpu_temp_c);
        self.audio.refresh();
        self.display.refresh();
        self.last_refresh = Instant::now();

        // Update overview history (push current values)
        if let Some(t) = self.system.cpu_temp_c {
            self.hist_cpu_temp.push((t.round() as i64).max(0) as u64);
        }
        if let Some(t) = self.system.gpu_temp_c {
            self.hist_gpu_temp.push((t.round() as i64).max(0) as u64);
        }
        if let Some(p) = self.system.mem_used_percent() {
            self.hist_mem_pct.push(p.round() as u64);
        }
        self.hist_bat_pct.push(self.battery.info.capacity as u64);
        if let Some(r) = self.cpu.fan_cpu_rpm {
            self.hist_fan_cpu.push(r as u64);
        }
        if let Some(r) = self.cpu.fan_gpu_rpm {
            self.hist_fan_gpu.push(r as u64);
        }
        if let Some(p) = self.battery.info.power_w {
            self.hist_power.push((p * 10.0).round() as u64);
        }
        self.needs_redraw = true;
    }

    fn expire_status(&mut self) {
        if let Some(until) = self.status_until {
            if Instant::now() > until {
                self.status = Status::None;
                self.status_until = Option::None;
                self.needs_redraw = true;
            }
        }
    }

    fn ok(&mut self, msg: impl Into<String>, secs: u64) {
        self.status = Status::Ok(msg.into());
        self.status_until = Some(Instant::now() + Duration::from_secs(secs));
        self.needs_redraw = true;
    }

    fn err(&mut self, msg: impl Into<String>, secs: u64) {
        self.status = Status::Err(msg.into());
        self.status_until = Some(Instant::now() + Duration::from_secs(secs));
        self.needs_redraw = true;
    }

    fn do_apply(&mut self) {
        match self.battery.set_charge_limit(self.desired_limit) {
            Ok(()) => {
                self.battery.refresh();
                self.config.charge_limit = self.desired_limit;
                self.config.save();
                self.ok(format!("limit set to {}%", self.desired_limit), 4);
            }
            Err(e) => self.err(e, 8),
        }
    }

    fn do_setup(&mut self) {
        match self.battery.run_setup(self.desired_limit) {
            Ok(()) => {
                self.battery.refresh();
                self.config.persistent_limit = true;
                self.config.save();
                self.ok("setup complete — persistent limit active", 6);
            }
            Err(e) => self.err(e, 10),
        }
    }

    fn do_persist(&mut self) {
        match self.battery.update_persistent_limit(self.desired_limit) {
            Ok(()) => self.ok("persistent limit updated", 4),
            Err(e) => self.err(e, 8),
        }
    }

    fn do_apply_gpu(&mut self) {
        match self.gpu.queue_gpu_mode() {
            Ok(()) => self.ok(format!("{} queued - reboot to apply", self.gpu.pending_mode.label()), 6),
            Err(e) => self.err(e, 8),
        }
    }

    // ── Input ─────────────────────────────────────────────────────────────────

    fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        self.needs_redraw = true;
        // Global
        match code {
            KeyCode::Char('q') | KeyCode::Char('Q') => { self.should_quit = true; return; }
            KeyCode::Char('r') | KeyCode::Char('R') => { self.refresh(); return; }
            KeyCode::Tab => {
                // In audio tab, Tab switches between speakers/mics
                if self.active_tab == 5 && self.focus == Focus::Content {
                    // fall through to content_key
                } else {
                    self.focus = if self.focus == Focus::Sidebar { Focus::Content } else { Focus::Sidebar };
                    return;
                }
            }
            _ => {}
        }
        match self.focus {
            Focus::Sidebar  => self.sidebar_key(code),
            Focus::Content  => self.content_key(code, mods),
        }
    }

    fn sidebar_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Down  | KeyCode::Char('j') => self.active_tab = (self.active_tab + 1).min(7),
            KeyCode::Up    | KeyCode::Char('k') => { if self.active_tab > 0 { self.active_tab -= 1; } }
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => self.focus = Focus::Content,
            _ => {}
        }
    }

    fn content_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc | KeyCode::BackTab => { self.focus = Focus::Sidebar; return; }
            _ => {}
        }
        match self.active_tab {
            0 => {} // overview tab -- read-only dashboard
            1 => self.battery_key(code, mods),
            2 => {} // system tab -- read-only
            3 => self.cpu_key(code),
            4 => self.gpu_key(code),
            5 => self.audio_key(code),
            6 => self.display_key(code),
            7 => self.settings_key(code),
            _ => {}
        }
    }

    fn battery_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let step: u8 = if mods.contains(KeyModifiers::SHIFT) { 5 } else { 1 };
        match code {
            KeyCode::Left  => self.desired_limit = self.desired_limit.saturating_sub(step).max(20),
            KeyCode::Right => self.desired_limit = (self.desired_limit as u16 + step as u16).min(100) as u8,
            KeyCode::Char('1') => self.desired_limit = 60,
            KeyCode::Char('2') => self.desired_limit = 80,
            KeyCode::Char('3') => self.desired_limit = 100,
            KeyCode::Enter | KeyCode::Char('a') => self.do_apply(),
            KeyCode::Char('s') => {
                if self.battery.info.persistent_enabled { self.do_persist(); } else { self.do_setup(); }
            }
            _ => {}
        }
    }

    fn gpu_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Left => {
                let variants = GpuMode::variants(self.gpu.has_mux);
                let idx = self.gpu.pending_mode.index(self.gpu.has_mux);
                if idx > 0 {
                    self.gpu.pending_mode = variants[idx - 1];
                }
            }
            KeyCode::Right => {
                let variants = GpuMode::variants(self.gpu.has_mux);
                let idx = self.gpu.pending_mode.index(self.gpu.has_mux);
                if idx < variants.len() - 1 {
                    self.gpu.pending_mode = variants[idx + 1];
                }
            }
            KeyCode::Enter | KeyCode::Char('a') => self.do_apply_gpu(),
            _ => {}
        }
    }

    // ── CPU tab ──────────────────────────────────────────────────────────────

    fn cpu_key(&mut self, code: KeyCode) {
        // Sections: 0 = thermal profile, 1 = boost
        let max_section = if self.cpu.asus_wmi_available { 1 } else { 1 };
        match code {
            KeyCode::Down  | KeyCode::Char('j') => { if self.cpu_section < max_section { self.cpu_section += 1; } }
            KeyCode::Up    | KeyCode::Char('k') => { if self.cpu_section > 0 { self.cpu_section -= 1; } }
            KeyCode::Left => self.cpu_left(),
            KeyCode::Right => self.cpu_right(),
            KeyCode::Enter | KeyCode::Char('a') => self.cpu_apply(),
            KeyCode::Char(' ') => {
                if self.cpu_section == 1 {
                    self.cpu.pending_boost = !self.cpu.pending_boost;
                    self.cpu_apply();
                }
            }
            _ => {}
        }
    }

    fn cpu_left(&mut self) {
        match self.cpu_section {
            0 => {
                let variants = ThermalProfile::variants();
                let idx = self.cpu.pending_thermal.index().saturating_sub(1);
                self.cpu.pending_thermal = variants[idx];
            }
            1 => {
                self.cpu.pending_boost = false;
            }
            _ => {}
        }
    }

    fn cpu_right(&mut self) {
        match self.cpu_section {
            0 => {
                let variants = ThermalProfile::variants();
                let idx = (self.cpu.pending_thermal.index() + 1).min(variants.len() - 1);
                self.cpu.pending_thermal = variants[idx];
            }
            1 => {
                self.cpu.pending_boost = true;
            }
            _ => {}
        }
    }

    fn cpu_apply(&mut self) {
        let result = match self.cpu_section {
            0 => {
                let label = self.cpu.pending_thermal.label().to_string();
                self.cpu.apply_thermal_profile().map(|()| format!("{label} thermal profile active"))
            }
            1 => {
                let on = self.cpu.pending_boost;
                self.cpu.apply_boost().map(|()| {
                    self.config.boost_enabled = Some(on);
                    self.config.save();
                    format!("CPU boost {}", if on { "enabled" } else { "disabled" })
                })
            }
            _ => Ok(String::new()),
        };
        match result {
            Ok(msg) => self.ok(msg, 4),
            Err(e) => self.err(e, 8),
        }
    }

    fn audio_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab => {
                self.audio.section = if self.audio.section == 0 { 1 } else { 0 };
                self.audio.cursor = 0;
            }
            KeyCode::Down  | KeyCode::Char('j') => {
                let total = self.audio.total_rows();
                if total > 0 { self.audio.cursor = (self.audio.cursor + 1).min(total - 1); }
            }
            KeyCode::Up    | KeyCode::Char('k') => {
                if self.audio.cursor > 0 { self.audio.cursor -= 1; }
            }
            KeyCode::Left => {
                match self.audio.set_volume(-5) {
                    Ok(()) => self.audio.refresh(),
                    Err(e) => self.err(e, 4),
                }
            }
            KeyCode::Right => {
                match self.audio.set_volume(5) {
                    Ok(()) => self.audio.refresh(),
                    Err(e) => self.err(e, 4),
                }
            }
            KeyCode::Char(' ') => {
                match self.audio.toggle_mute() {
                    Ok(()) => { self.audio.refresh(); self.ok("toggled mute", 2); }
                    Err(e) => self.err(e, 4),
                }
            }
            KeyCode::Enter => {
                let (_, port_idx) = self.audio.cursor_info();
                if port_idx.is_some() {
                    // Cursor is on a port -- activate it
                    match self.audio.activate_port() {
                        Ok(()) => { self.audio.refresh(); self.ok("port activated", 3); }
                        Err(e) => self.err(e, 4),
                    }
                } else {
                    // Cursor is on device header -- toggle mute
                    match self.audio.toggle_mute() {
                        Ok(()) => { self.audio.refresh(); self.ok("toggled mute", 2); }
                        Err(e) => self.err(e, 4),
                    }
                }
            }
            KeyCode::Char('d') => {
                let kind = if self.audio.section == 0 { "output" } else { "input" };
                match self.audio.set_default() {
                    Ok(()) => { self.audio.refresh(); self.ok(format!("default {kind} set"), 3); }
                    Err(e) => self.err(e, 4),
                }
            }
            _ => {}
        }
    }

    fn display_key(&mut self, code: KeyCode) {
        if !self.display.available {
            return;
        }
        let modes = self.display.current_monitor_modes();
        if modes.is_empty() {
            return;
        }
        let max_idx = modes.len().saturating_sub(1);
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.display.pending_mode_idx > 0 {
                    self.display.pending_mode_idx -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.display.pending_mode_idx < max_idx {
                    self.display.pending_mode_idx += 1;
                }
            }
            KeyCode::Enter | KeyCode::Char('a') => {
                if let Some(mode) = modes.get(self.display.pending_mode_idx) {
                    let mode_id = mode.mode_id.clone();
                    let (w, h, rate) = (mode.width, mode.height, mode.refresh_rate);
                    match self.display.apply_refresh_rate(&mode_id) {
                        Ok(()) => {
                            self.ok(format!("switched to {w}x{h} @ {rate:.0}Hz"), 4);
                        }
                        Err(e) => self.err(e, 8),
                    }
                }
            }
            _ => {}
        }
    }

    fn settings_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Down  | KeyCode::Char('j') => self.settings_cursor = (self.settings_cursor + 1).min(4),
            KeyCode::Up    | KeyCode::Char('k') => { if self.settings_cursor > 0 { self.settings_cursor -= 1; } }
            KeyCode::Left  => match self.settings_cursor {
                0 => { self.config.charge_limit = self.config.charge_limit.saturating_sub(1).max(20); self.desired_limit = self.config.charge_limit; self.config.save(); }
                2 => { if self.config.refresh_secs > 1 { self.config.refresh_secs -= 1; self.config.save(); } }
                _ => {}
            },
            KeyCode::Right => match self.settings_cursor {
                0 => { self.config.charge_limit = self.config.charge_limit.saturating_add(1).min(100); self.desired_limit = self.config.charge_limit; self.config.save(); }
                2 => { if self.config.refresh_secs < 60 { self.config.refresh_secs += 1; self.config.save(); } }
                _ => {}
            },
            KeyCode::Enter | KeyCode::Char(' ') => match self.settings_cursor {
                1 => { self.config.auto_apply_on_start = !self.config.auto_apply_on_start; self.config.save(); }
                3 => {
                    let new_val = !self.config.boost_enabled.unwrap_or(false);
                    self.config.boost_enabled = Some(new_val);
                    if self.cpu.boost_available {
                        self.cpu.pending_boost = new_val;
                        let _ = self.cpu.apply_boost();
                    }
                    self.config.save();
                }
                4 => self.do_setup(),
                _ => {}
            },
            _ => {}
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Ensure terminal is restored even on panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, cursor::Show);
        original_hook(info);
    }));

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, cursor::Hide)?;

    let backend  = CrosstermBackend::new(stdout());
    let mut term = Terminal::new(backend)?;
    let mut app  = TuiApp::new();

    loop {
        if app.needs_redraw {
            term.draw(|f| render(f, &app))?;
            app.needs_redraw = false;
        }

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(KeyEvent { code, kind: KeyEventKind::Press, modifiers, .. }) = event::read()? {
                app.handle_key(code, modifiers);
                if app.should_quit { break; }
            }
        }

        if app.last_refresh.elapsed() >= Duration::from_secs(app.config.refresh_secs) {
            app.refresh();
        }
        app.expire_status();
    }

    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen, cursor::Show)?;
    term.show_cursor()?;
    Ok(())
}

// ── Top-level render ──────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &TuiApp) {
    let area = f.area();

    // Outer vertical: title | main | statusbar
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ]).split(area);

    render_titlebar(f, app, rows[0]);
    render_statusbar(f, app, rows[2]);

    // Main horizontal: sidebar | content
    let cols = Layout::horizontal([
        Constraint::Length(18),
        Constraint::Fill(1),
    ]).split(rows[1]);

    render_sidebar(f, app, cols[0]);

    match app.active_tab {
        0 => render_overview(f, app, cols[1]),
        1 => render_battery(f, app, cols[1]),
        2 => render_system(f, app, cols[1]),
        3 => render_cpu(f, app, cols[1]),
        4 => render_gpu(f, app, cols[1]),
        5 => render_audio(f, app, cols[1]),
        6 => render_display(f, app, cols[1]),
        7 => render_settings(f, app, cols[1]),
        _ => {}
    }
}

// ── Title bar ─────────────────────────────────────────────────────────────────

fn render_titlebar(f: &mut Frame, app: &TuiApp, area: Rect) {
    let bat = &app.battery.info;
    let sys = &app.system;
    let bg  = Style::default().bg(Color::Rgb(11, 11, 11));

    // Left side
    let mut left: Vec<Span> = vec![
        Span::styled("  zhelper", Style::default().fg(GREEN).add_modifier(Modifier::BOLD).bg(Color::Rgb(11, 11, 11))),
        Span::styled("  ·  ", fnt().bg(Color::Rgb(11, 11, 11))),
        Span::styled(
            format!("bat {}%", bat.capacity),
            if bat.status.is_plugged() { g() } else if bat.capacity < 20 { dng() } else { dim() }
                .bg(Color::Rgb(11, 11, 11)),
        ),
    ];
    if let Some(t) = sys.cpu_temp_c {
        let col = if t >= 80.0 { DANGER } else { DIM };
        left.push(Span::styled("  ·  ", fnt().bg(Color::Rgb(11, 11, 11))));
        left.push(Span::styled(format!("cpu {t:.0}°c"), Style::default().fg(col).bg(Color::Rgb(11, 11, 11))));
    }
    if let Some(t) = sys.gpu_temp_c {
        let col = if t >= 85.0 { DANGER } else { DIM };
        left.push(Span::styled("  ·  ", fnt().bg(Color::Rgb(11, 11, 11))));
        left.push(Span::styled(format!("gpu {t:.0}°c"), Style::default().fg(col).bg(Color::Rgb(11, 11, 11))));
    }

    f.render_widget(Paragraph::new(Line::from(left)).style(bg), area);

    // Right side — AC status
    let ac = if sys.ac_connected { "ac  " } else { "bat  " };
    f.render_widget(
        Paragraph::new(Span::styled(ac, dim().bg(Color::Rgb(11, 11, 11))))
            .alignment(Alignment::Right)
            .style(bg),
        area,
    );
}

// ── Sidebar ───────────────────────────────────────────────────────────────────

fn render_sidebar(f: &mut Frame, app: &TuiApp, area: Rect) {
    let focused  = app.focus == Focus::Sidebar;
    let bdr      = if focused { Color::Rgb(45, 45, 45) } else { LINE };
    let tabs     = ["overview", "battery", "system", "cpu", "gpu", "audio", "display", "settings"];

    let items: Vec<ListItem> = tabs.iter()
        .map(|&t| ListItem::new(format!("  {t}")).style(dim()))
        .collect();

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(bdr));

    let list = List::new(items)
        .block(block)
        .highlight_style(bg())
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    state.select(Some(app.active_tab));
    f.render_stateful_widget(list, area, &mut state);
}

// ── Overview tab ──────────────────────────────────────────────────────────────

fn render_overview(f: &mut Frame, app: &TuiApp, area: Rect) {
    let sys = &app.system;
    let bat = &app.battery.info;

    // Layout: summary strip on top, then a 2-column grid of charts
    let rows = Layout::vertical([
        Constraint::Length(1),   // summary strip
        Constraint::Fill(1),     // charts
    ]).split(area);

    // ── Summary strip ────────────────────────────────────────────────────────
    let mut summary: Vec<Span> = vec![
        Span::styled("overview", Style::default().fg(GREEN).add_modifier(Modifier::BOLD)),
        Span::styled("  ·  ", fnt()),
    ];
    if let Some(ref m) = sys.cpu_model {
        let s: String = m.chars().take(40).collect();
        summary.push(Span::styled(s, dim()));
        summary.push(Span::styled("  ·  ", fnt()));
    }
    let ac = if sys.ac_connected { "AC" } else { "BAT" };
    let ac_col = if sys.ac_connected { GREEN } else { DIM };
    summary.push(Span::styled(ac, Style::default().fg(ac_col)));
    f.render_widget(
        Paragraph::new(Line::from(summary)).style(Style::default().bg(Color::Rgb(11, 11, 11))),
        rows[0],
    );

    // ── Chart grid ───────────────────────────────────────────────────────────
    // 4 rows x 2 columns
    let chart_rows = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ]).split(rows[1]);

    let row0 = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(chart_rows[0]);
    let row1 = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(chart_rows[1]);
    let row2 = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(chart_rows[2]);
    let row3 = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(chart_rows[3]);

    // helper: render one chart card with inline title+value and a full-height sparkline
    let render_chart = |f: &mut Frame, area: Rect, title: &str, current: Option<String>, hist: &History, color: Color, max: u64| {
        let val_text = current.clone().unwrap_or_else(|| "—".to_string());
        let title_line = Line::from(vec![
            Span::styled(format!(" {title} "), fnt()),
            Span::styled(val_text, Style::default().fg(color).add_modifier(Modifier::BOLD)),
        ]);

        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Rgb(35, 35, 35)))
            .title_top(title_line)
            .style(Style::default().bg(Color::Rgb(14, 14, 14)));

        let inner = block.inner(area);
        f.render_widget(block, area);

        if inner.height < 1 || inner.width < 1 { return; }

        let data = hist.data();
        if !data.is_empty() {
            let spark = Sparkline::default()
                .data(data.as_slice())
                .max(max)
                .direction(ratatui::widgets::RenderDirection::RightToLeft)
                .style(Style::default().fg(color).bg(Color::Rgb(14, 14, 14)));
            f.render_widget(spark, inner);
        } else if current.is_none() {
            f.render_widget(
                Paragraph::new(Span::styled(" no data", fnt()))
                    .style(Style::default().bg(Color::Rgb(14, 14, 14))),
                inner,
            );
        }
    };

    // CPU temp
    let cpu_cur = sys.cpu_temp_c.map(|t| format!("{:.0}°C", t));
    let cpu_color = sys.cpu_temp_c
        .map(|t| if t >= 85.0 { DANGER } else if t >= 70.0 { Color::Rgb(220, 180, 70) } else { GREEN })
        .unwrap_or(GREEN);
    let cpu_max = app.hist_cpu_temp.data().iter().copied().max().unwrap_or(100).max(100);
    render_chart(f, row0[0], "cpu temp", cpu_cur, &app.hist_cpu_temp, cpu_color, cpu_max);

    // GPU temp
    let gpu_cur = sys.gpu_temp_c.map(|t| format!("{:.0}°C", t));
    let gpu_color = sys.gpu_temp_c
        .map(|t| if t >= 88.0 { DANGER } else if t >= 75.0 { Color::Rgb(220, 180, 70) } else { GREEN })
        .unwrap_or(GREEN);
    let gpu_max = app.hist_gpu_temp.data().iter().copied().max().unwrap_or(100).max(100);
    render_chart(f, row0[1], "gpu temp", gpu_cur, &app.hist_gpu_temp, gpu_color, gpu_max);

    // Memory %
    let mem_cur = sys.mem_used_percent().map(|p| format!("{:.0}%", p));
    let mem_color = sys.mem_used_percent()
        .map(|p| if p > 85.0 { DANGER } else if p > 65.0 { Color::Rgb(220, 180, 70) } else { GREEN })
        .unwrap_or(GREEN);
    render_chart(f, row1[0], "memory", mem_cur, &app.hist_mem_pct, mem_color, 100);

    // Battery %
    let bat_val = format!("{}%", bat.capacity);
    let bat_color = if bat.capacity < 20 && !bat.status.is_plugged() {
        DANGER
    } else if bat.status.is_plugged() {
        Color::Rgb(100, 200, 255)
    } else {
        GREEN
    };
    render_chart(f, row1[1], "battery", Some(bat_val), &app.hist_bat_pct, bat_color, 100);

    // CPU fan RPM
    let fan_cpu_cur = app.cpu.fan_cpu_rpm.map(|r| format!("{} rpm", r));
    let fan_cpu_max = app.hist_fan_cpu.data().iter().copied().max().unwrap_or(5000).max(3000);
    render_chart(f, row2[0], "cpu fan", fan_cpu_cur, &app.hist_fan_cpu, GREEN, fan_cpu_max);

    // GPU fan RPM
    let fan_gpu_cur = app.cpu.fan_gpu_rpm.map(|r| format!("{} rpm", r));
    let fan_gpu_max = app.hist_fan_gpu.data().iter().copied().max().unwrap_or(5000).max(3000);
    render_chart(f, row2[1], "gpu fan", fan_gpu_cur, &app.hist_fan_gpu, GREEN, fan_gpu_max);

    // Power draw (battery)
    let power_cur = bat.power_w.map(|p| format!("{:.1} W", p));
    let power_max = app.hist_power.data().iter().copied().max().unwrap_or(20).max(5);
    render_chart(f, row3[0], "power draw", power_cur, &app.hist_power, GREEN, power_max);
}

// ── Battery tab ───────────────────────────────────────────────────────────────

fn render_battery(f: &mut Frame, app: &TuiApp, area: Rect) {
    let bat = &app.battery.info;
    let focused = app.focus == Focus::Content;

    let rows = Layout::vertical([
        Constraint::Length(5),  // status
        Constraint::Length(8),  // charge limit
        Constraint::Length(5),  // details
        Constraint::Fill(1),
    ]).split(area);

    // ── Status block ─────────────────────────────────────────────────────────
    {
        let bdr = if focused { Color::Rgb(40, 40, 40) } else { LINE };
        let block = Block::default()
            .title(Span::styled(" battery status ", dim()))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(bdr));
        let inner = block.inner(rows[0]);
        f.render_widget(block, rows[0]);

        let cells = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]).split(inner);

        let arc = if bat.status.is_plugged() { GREEN }
                  else if bat.capacity < 20   { DANGER }
                  else                         { GREEN };

        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(arc).bg(Color::Rgb(22, 22, 22)))
            .percent(bat.capacity as u16)
            .label(Span::styled(
                format!("{:3}%  {}", bat.capacity, bat.status.label()),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ));
        f.render_widget(gauge, cells[0]);

        // Stats row
        let mut spans: Vec<Span> = vec![];
        macro_rules! stat {
            ($label:expr, $val:expr) => {
                spans.push(Span::styled($label, dim()));
                spans.push(Span::styled(format!("{}  ", $val), txt()));
            };
        }
        if let Some(v) = bat.voltage_v { stat!("voltage ", format!("{v:.1}V")); }
        if let Some(c) = bat.current_a { stat!("current ", format!("{:.1}A", c.abs())); }
        if let Some(p) = bat.power_w   { stat!("power ",   format!("{p:.1}W")); }
        if let Some(h) = bat.time_remaining_h() {
            let lbl = if matches!(bat.status, ChargeStatus::Charging) { "to full " } else { "remaining " };
            let hh = h as u32;
            let mm = ((h - hh as f32) * 60.0) as u32;
            stat!(lbl, format!("{hh}h {mm:02}m"));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), cells[2]);
    }

    // ── Charge limit block ────────────────────────────────────────────────────
    {
        let changed  = app.desired_limit != bat.charge_limit;
        let bdr      = if focused { Color::Rgb(35, 60, 45) } else { LINE };
        let title_col = if focused { GREEN } else { DIM };

        let block = Block::default()
            .title(Span::styled(
                " charge limit ",
                Style::default().fg(title_col),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(bdr));
        let inner = block.inner(rows[1]);
        f.render_widget(block, rows[1]);

        let cells = Layout::vertical([
            Constraint::Length(1), // gauge
            Constraint::Length(1), // spacer
            Constraint::Length(1), // presets
            Constraint::Length(1), // spacer
            Constraint::Length(1), // action / status
            Constraint::Fill(1),
        ]).split(inner);

        // Gauge
        let g_col   = if changed { GREEN } else { Color::Rgb(48, 48, 48) };
        let hint    = if focused { "  ←→ adjust  shift+←→ ±5" } else { "  (focus with tab)" };
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(g_col).bg(Color::Rgb(22, 22, 22)))
            .percent(app.desired_limit as u16)
            .label(Span::styled(
                format!("{:3}%{hint}", app.desired_limit),
                if changed { txt() } else { dim() },
            ));
        f.render_widget(gauge, cells[0]);

        // Preset buttons
        let mut preset_spans: Vec<Span> = vec![Span::styled("  presets  ", fnt())];
        for (i, &pct) in [60u8, 80, 100].iter().enumerate() {
            let active = app.desired_limit == pct;
            let s = if active { format!("[{pct}%]") } else { format!(" {pct}% ") };
            preset_spans.push(Span::styled(s, if active { bg() } else { dim() }));
            if i < 2 { preset_spans.push(Span::styled("  ", fnt())); }
        }
        preset_spans.push(Span::styled("   1 / 2 / 3", fnt()));
        f.render_widget(Paragraph::new(Line::from(preset_spans)), cells[2]);

        // Action line or status
        let action_widget = match &app.status {
            Status::Ok(msg)  => Paragraph::new(Line::from(vec![
                Span::styled("  ✓  ", g()),
                Span::styled(msg.as_str(), g()),
            ])),
            Status::Err(msg) => Paragraph::new(Line::from(vec![
                Span::styled("  ✗  ", dng()),
                Span::styled(msg.as_str(), dng()),
            ])),
            Status::None => {
                let apply_style = if changed { g() } else { dim() };
                let persist_label = if app.battery.info.persistent_enabled { "[s] update persist" } else { "[s] setup persist" };
                Paragraph::new(Line::from(vec![
                    Span::styled("  [a/↵] apply  ", apply_style),
                    Span::styled(persist_label, fnt()),
                ]))
            }
        };
        f.render_widget(action_widget, cells[4]);
    }

    // ── Details block ─────────────────────────────────────────────────────────
    {
        let block = Block::default()
            .title(Span::styled(" details ", fnt()))
            .borders(Borders::ALL)
            .border_style(line_border());
        let inner = block.inner(rows[2]);
        f.render_widget(block, rows[2]);

        let cells = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
        ]).split(inner);

        // Row 1: energy + health
        let mut r1: Vec<Span> = vec![];
        if let Some(en) = bat.energy_now_wh  { r1.push(Span::styled("energy now ",  dim())); r1.push(Span::styled(format!("{en:.1}Wh  "), txt())); }
        if let Some(ef) = bat.energy_full_wh { r1.push(Span::styled("full ",        dim())); r1.push(Span::styled(format!("{ef:.1}Wh  "), txt())); }
        if let Some(h)  = bat.health_percent() {
            let hc = if h > 80.0 { GREEN } else if h > 60.0 { DIM } else { DANGER };
            r1.push(Span::styled("health ", dim()));
            r1.push(Span::styled(format!("{h:.0}%"), Style::default().fg(hc)));
        }
        f.render_widget(Paragraph::new(Line::from(r1)), cells[0]);

        // Row 2: cycles + tech + device + persistent
        let mut r2: Vec<Span> = vec![];
        if let Some(c) = bat.cycle_count { r2.push(Span::styled("cycles ", dim())); r2.push(Span::styled(format!("{c}  "), txt())); }
        if let Some(t) = &bat.technology { r2.push(Span::styled("tech ", dim())); r2.push(Span::styled(format!("{t}  "), txt())); }
        r2.push(Span::styled("device ", dim()));
        r2.push(Span::styled(format!("{}  ", app.battery.bat_name()), txt()));
        r2.push(Span::styled("persistent ", dim()));
        r2.push(Span::styled(
            if bat.persistent_enabled { "active" } else { "not set" },
            if bat.persistent_enabled { g() } else { dim() },
        ));
        f.render_widget(Paragraph::new(Line::from(r2)), cells[1]);
    }
}

// ── System tab ────────────────────────────────────────────────────────────────

fn render_system(f: &mut Frame, app: &TuiApp, area: Rect) {
    let sys = &app.system;

    let rows = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Fill(1),
    ]).split(area);

    // CPU / GPU temps
    {
        let block = Block::default()
            .title(Span::styled(" processor ", fnt()))
            .borders(Borders::ALL)
            .border_style(line_border());
        let inner = block.inner(rows[0]);
        f.render_widget(block, rows[0]);

        let cells = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]).split(inner);

        if let Some(ref m) = sys.cpu_model {
            let s: String = m.chars().take((inner.width as usize).saturating_sub(2)).collect();
            f.render_widget(Paragraph::new(Span::styled(s, dim())), cells[0]);
        }

        let mut spans: Vec<Span> = vec![];
        if let Some(t) = sys.cpu_temp_c {
            let col = if t >= 80.0 { DANGER } else { TEXT };
            spans.push(Span::styled("cpu ", dim()));
            spans.push(Span::styled(format!("{t:.0}°C"), Style::default().fg(col)));
        }
        if let Some(t) = sys.gpu_temp_c {
            let col = if t >= 85.0 { DANGER } else { TEXT };
            spans.push(Span::styled("   gpu ", dim()));
            spans.push(Span::styled(format!("{t:.0}°C"), Style::default().fg(col)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), cells[2]);
    }

    // Memory
    {
        let block = Block::default()
            .title(Span::styled(" memory ", fnt()))
            .borders(Borders::ALL)
            .border_style(line_border());
        let inner = block.inner(rows[1]);
        f.render_widget(block, rows[1]);

        if let (Some(total), Some(used), Some(avail)) =
            (sys.mem_total_mb, sys.mem_used_mb, sys.mem_available_mb)
        {
            let pct = sys.mem_used_percent().unwrap_or(0.0);
            let mc  = if pct > 85.0 { DANGER } else { GREEN };

            let cells = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ]).split(inner);

            let gauge = Gauge::default()
                .gauge_style(Style::default().fg(mc).bg(Color::Rgb(22, 22, 22)))
                .percent(pct as u16)
                .label(Span::styled(format!("{pct:.0}%  ({used} / {total} MB)"), txt()));
            f.render_widget(gauge, cells[0]);

            f.render_widget(Paragraph::new(Line::from(vec![
                Span::styled("used ",      dim()), Span::styled(format!("{used} MB  "),  txt()),
                Span::styled("available ", dim()), Span::styled(format!("{avail} MB  "), txt()),
                Span::styled("total ",     dim()), Span::styled(format!("{total} MB"),   txt()),
            ])), cells[2]);
        } else {
            f.render_widget(Paragraph::new(Span::styled("unavailable", dim())), inner);
        }
    }

    // Thermal zones
    if !sys.thermal_zones.is_empty() {
        let block = Block::default()
            .title(Span::styled(" thermal zones ", fnt()))
            .borders(Borders::ALL)
            .border_style(line_border());
        let inner = block.inner(rows[2]);
        f.render_widget(block, rows[2]);

        let cols = Layout::horizontal([
            Constraint::Ratio(1, 2),
            Constraint::Ratio(1, 2),
        ]).split(inner);

        let half = (sys.thermal_zones.len() + 1) / 2;
        for (ci, chunk) in sys.thermal_zones.chunks(half).enumerate() {
            if ci >= 2 { break; }
            let lines: Vec<Line> = chunk.iter().map(|z| {
                let tc = if z.temp_c >= 80.0 { DANGER } else if z.temp_c >= 65.0 { DIM } else { TEXT };
                Line::from(vec![
                    Span::styled(format!("{:<22}", z.name), dim()),
                    Span::styled(format!("{:.1}°C", z.temp_c), Style::default().fg(tc)),
                ])
            }).collect();
            f.render_widget(Paragraph::new(Text::from(lines)), cols[ci]);
        }
    }
}

// ── CPU tab ───────────────────────────────────────────────────────────────────

fn render_cpu(f: &mut Frame, app: &TuiApp, area: Rect) {
    let focused = app.focus == Focus::Content;
    let cpu = &app.cpu;

    let rows = Layout::vertical([
        Constraint::Length(5),  // status: temp + fans
        Constraint::Length(7),  // thermal profile
        Constraint::Length(5),  // boost
        Constraint::Fill(1),    // descriptions / status
    ]).split(area);

    // ── Status block ─────────────────────────────────────────────────────────
    {
        let bdr = if focused { Color::Rgb(40, 40, 40) } else { LINE };
        let block = Block::default()
            .title(Span::styled(" cpu status ", dim()))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(bdr));
        let inner = block.inner(rows[0]);
        f.render_widget(block, rows[0]);

        let cells = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
        ]).split(inner);

        let mut spans: Vec<Span> = vec![];
        if let Some(t) = cpu.cpu_temp_c {
            let col = if t >= 85.0 { DANGER } else if t >= 70.0 { DIM } else { TEXT };
            spans.push(Span::styled("cpu ", dim()));
            spans.push(Span::styled(format!("{t:.0}°C"), Style::default().fg(col)));
        }
        if let Some(rpm) = cpu.fan_cpu_rpm {
            spans.push(Span::styled("   cpu fan ", dim()));
            spans.push(Span::styled(format!("{rpm} RPM"), txt()));
        }
        if let Some(rpm) = cpu.fan_gpu_rpm {
            spans.push(Span::styled("   gpu fan ", dim()));
            spans.push(Span::styled(format!("{rpm} RPM"), txt()));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), cells[0]);

        let r2 = Line::from(vec![
            Span::styled("boost ", dim()),
            Span::styled(
                if cpu.boost_enabled { "on" } else { "off" },
                if cpu.boost_enabled { g() } else { fnt() },
            ),
        ]);
        f.render_widget(Paragraph::new(r2), cells[1]);
    }

    // ── Thermal profile ───────────────────────────────────────────────────────
    if cpu.asus_wmi_available {
        let sec_focused = focused && app.cpu_section == 0;
        let bdr = if sec_focused { Color::Rgb(35, 60, 45) } else { LINE };
        let title_col = if sec_focused { GREEN } else { DIM };
        let block = Block::default()
            .title(Span::styled(" thermal profile (bios) ", Style::default().fg(title_col)))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(bdr));
        let inner = block.inner(rows[1]);
        f.render_widget(block, rows[1]);

        let cells = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]).split(inner);

        let mut spans: Vec<Span> = vec![Span::styled("  profile  ", fnt())];
        for p in ThermalProfile::variants() {
            let is_active = p == cpu.thermal_profile;
            let is_pending = p == cpu.pending_thermal;
            let label = if is_pending { format!("[{}]", p.label()) } else { format!(" {} ", p.label()) };
            let style = match (is_active, is_pending) {
                (true, true) => bg(),
                (false, true) => txt().add_modifier(Modifier::BOLD),
                (true, false) => g(),
                (false, false) => dim(),
            };
            spans.push(Span::styled(label, style));
            spans.push(Span::styled("  ", fnt()));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), cells[0]);
        f.render_widget(Paragraph::new(Span::styled(
            format!("  {}", cpu.pending_thermal.description()), dim()
        )), cells[1]);
        f.render_widget(Paragraph::new(Span::styled("  current value maps to bios fan curve + power limits", fnt())), cells[3]);
    }

    // ── Boost ─────────────────────────────────────────────────────────────────
    {
        let bdr = if focused { Color::Rgb(35, 35, 35) } else { LINE };
        let block = Block::default()
            .title(Span::styled(" cpu boost ", Style::default().fg(if focused { DIM } else { FAINT })))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(bdr));
        let inner = block.inner(rows[2]);
        f.render_widget(block, rows[2]);

        let sel = focused && app.cpu_section == 1;
        let cursor = if sel { "▶ " } else { "  " };
        let mut spans = vec![Span::styled(cursor, if sel { g() } else { fnt() })];
        spans.push(Span::styled("cpu boost ", dim()));
        let is_pending = cpu.pending_boost;
        let is_active = cpu.boost_enabled;
        let style = match (is_active, is_pending) {
            (true, true) => bg(),
            (false, true) => txt().add_modifier(Modifier::BOLD),
            (true, false) => Style::default().fg(GREEN),
            (false, false) => dim(),
        };
        spans.push(Span::styled(
            if is_pending { "[on]  off" } else { " on  [off]" },
            style,
        ));
        f.render_widget(Paragraph::new(Line::from(spans)), inner);
    }

    // ── Description / status ────────────────────────────────────────────────
    {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(line_border());
        let inner = block.inner(rows[3]);
        f.render_widget(block, rows[3]);

        let status_active = !matches!(app.status, Status::None);
        let mut lines: Vec<Line> = vec![];

        if status_active {
            match &app.status {
                Status::Ok(msg) => lines.push(Line::from(vec![Span::styled(format!("  ✓  {msg}"), g())])),
                Status::Err(msg) => lines.push(Line::from(vec![Span::styled(format!("  ✗  {msg}"), dng())])),
                Status::None => {}
            }
        } else {
            let desc = match app.cpu_section {
                0 => cpu.pending_thermal.description(),
                1 => "Turbo boost allows CPU to exceed base clock. Disable to save power and reduce heat",
                _ => "",
            };
            if !desc.is_empty() {
                lines.push(Line::from(Span::styled(format!("  {desc}"), dim())));
            }
        }

        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }
}

// ── GPU tab ───────────────────────────────────────────────────────────────────

fn render_gpu(f: &mut Frame, app: &TuiApp, area: Rect) {
    let focused = app.focus == Focus::Content;

    let rows = Layout::vertical([
        Constraint::Length(7),  // GPU mode block
        Constraint::Fill(1),
    ]).split(area);

    let status_active = !matches!(app.status, Status::None);

    // ── GPU Mode block ────────────────────────────────────────────────────────
    {
        let bdr       = if focused { Color::Rgb(35, 60, 45) } else { LINE };
        let title_col = if focused { GREEN } else { DIM };

        let block = Block::default()
            .title(Span::styled(" graphics mode ", Style::default().fg(title_col)))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(bdr));
        let inner = block.inner(rows[0]);
        f.render_widget(block, rows[0]);

        let cells = Layout::vertical([
            Constraint::Length(1), // gpu names
            Constraint::Length(1), // selector row
            Constraint::Length(1), // description
            Constraint::Length(1), // spacer
            Constraint::Length(1), // action
            Constraint::Fill(1),
        ]).split(inner);

        // GPU names row (always shown)
        {
            let mut spans: Vec<Span> = vec![];
            if let Some(ref name) = app.gpu.igpu_name {
                spans.push(Span::styled("  iGPU  ", fnt()));
                spans.push(Span::styled(name.as_str(), dim()));
            }
            if let Some(ref name) = app.gpu.dgpu_name {
                spans.push(Span::styled("    dGPU  ", fnt()));
                spans.push(Span::styled(name.as_str(), dim()));
            }
            f.render_widget(Paragraph::new(Line::from(spans)), cells[0]);
        }

        if app.gpu.asus_wmi_available {
            // Selector row
            let mut spans: Vec<Span> = vec![Span::styled("  mode  ", fnt())];
            for mode in GpuMode::variants(app.gpu.has_mux) {
                let is_active  = *mode == app.gpu.mode;
                let is_pending = *mode == app.gpu.pending_mode;
                let is_queued  = app.gpu.queued_mode == Some(*mode);
                let label = if is_pending {
                    format!("[{}]", mode.label())
                } else if is_queued {
                    format!(" {}* ", mode.label())
                } else {
                    format!(" {} ", mode.label())
                };
                let style = match (is_active, is_pending) {
                    (true,  true)  => bg(),
                    (false, true)  => txt().add_modifier(Modifier::BOLD),
                    (true,  false) => g(),
                    (false, false) => dim(),
                };
                spans.push(Span::styled(label, style));
                spans.push(Span::styled("  ", fnt()));
            }
            if focused { spans.push(Span::styled("← →", fnt())); }
            f.render_widget(Paragraph::new(Line::from(spans)), cells[1]);

            // Description
            f.render_widget(
                Paragraph::new(Span::styled(format!("  {}", app.gpu.pending_mode.description()), dim())),
                cells[2],
            );

            // Queued indicator or action line
            let action = if status_active {
                match &app.status {
                    Status::Ok(msg)  => Paragraph::new(Line::from(vec![Span::styled("  ✓  ", g()),   Span::styled(msg.as_str(), g())])),
                    Status::Err(msg) => Paragraph::new(Line::from(vec![Span::styled("  ✗  ", dng()), Span::styled(msg.as_str(), dng())])),
                    Status::None     => Paragraph::new(Line::from(vec![])),
                }
            } else if let Some(queued) = app.gpu.queued_mode {
                if queued != app.gpu.mode {
                    Paragraph::new(Line::from(vec![
                        Span::styled("  ⏳  ", g()),
                        Span::styled(format!("{} queued - reboot to apply", queued.label()), g()),
                    ]))
                } else {
                    let changed = app.gpu.pending_mode != app.gpu.mode;
                    Paragraph::new(Line::from(vec![
                        Span::styled("  [a/↵] queue  ", if changed { g() } else { dim() }),
                        Span::styled("applies at shutdown, reboot required", fnt()),
                    ]))
                }
            } else {
                let changed = app.gpu.pending_mode != app.gpu.mode;
                Paragraph::new(Line::from(vec![
                    Span::styled("  [a/↵] queue  ", if changed { g() } else { dim() }),
                    Span::styled("applies at shutdown, reboot required", fnt()),
                ]))
            };
            f.render_widget(action, cells[4]);
        } else {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  ASUS GPU control not available  ", dng()),
                    Span::styled("requires asus-nb-wmi kernel module", dim()),
                ])),
                cells[1],
            );
        }
    }
}

// ── Audio tab ─────────────────────────────────────────────────────────────────

fn render_audio(f: &mut Frame, app: &TuiApp, area: Rect) {
    let focused = app.focus == Focus::Content;
    let active_section = app.audio.section;

    let rows = Layout::vertical([
        Constraint::Length(10), // output
        Constraint::Length(10), // input
        Constraint::Fill(1),    // status
    ]).split(area);

    let section_label = if active_section == 0 { "output" } else { "input" };

    // ── Output ───────────────────────────────────────────────────────────────
    {
        let sec_focused = focused && active_section == 0;
        let bdr = if sec_focused { Color::Rgb(35, 60, 45) } else { LINE };
        let title_col = if sec_focused { GREEN } else { DIM };
        let block = Block::default()
            .title(Span::styled(" output (speakers / headphones) ", Style::default().fg(title_col)))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(bdr));
        let inner = block.inner(rows[0]);
        f.render_widget(block, rows[0]);

        if app.audio.sinks.is_empty() {
            f.render_widget(Paragraph::new(Span::styled("  no output devices found", fnt())), inner);
        } else {
            let cursor = if active_section == 0 { app.audio.cursor } else { 999 };
            let lines = render_device_list(&app.audio.sinks, sec_focused, cursor);
            f.render_widget(Paragraph::new(Text::from(lines)), inner);
        }
    }

    // ── Input ────────────────────────────────────────────────────────────────
    {
        let sec_focused = focused && active_section == 1;
        let bdr = if sec_focused { Color::Rgb(35, 60, 45) } else { LINE };
        let title_col = if sec_focused { GREEN } else { DIM };
        let block = Block::default()
            .title(Span::styled(" input (microphones) ", Style::default().fg(title_col)))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(bdr));
        let inner = block.inner(rows[1]);
        f.render_widget(block, rows[1]);

        if app.audio.sources.is_empty() {
            f.render_widget(Paragraph::new(Span::styled("  no input devices found", fnt())), inner);
        } else {
            let cursor = if active_section == 1 { app.audio.cursor } else { 999 };
            let lines = render_device_list(&app.audio.sources, sec_focused, cursor);
            f.render_widget(Paragraph::new(Text::from(lines)), inner);
        }
    }

    // ── Status / hints ────────────────────────────────────────────────────────
    {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(line_border());
        let inner = block.inner(rows[2]);
        f.render_widget(block, rows[2]);

        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(
                format!("  section: {section_label}  -  tab to switch"),
                dim(),
            )),
            Line::from(""),
        ];

        if !matches!(app.status, Status::None) {
            match &app.status {
                Status::Ok(msg)  => lines.push(Line::from(vec![Span::styled(format!("  check {msg}"), g())])),
                Status::Err(msg) => lines.push(Line::from(vec![Span::styled(format!("  x  {msg}"), dng())])),
                Status::None => {}
            }
        } else {
            lines.push(Line::from(Span::styled(
                "  j/k navigate   enter activate port   space mute   d default",
                fnt(),
            )));
        }

        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }
}

fn render_device_list(devices: &[crate::audio::AudioDevice], focused: bool, cursor: usize) -> Vec<Line<'_>> {
    let mut lines: Vec<Line> = vec![];
    let mut row = 0usize;

    for dev in devices {
        // Device header row
        let sel = focused && row == cursor;
        let prefix = if sel { "> " } else { "  " };
        let default_mark = if dev.is_default { "* " } else { "  " };
        let mute_label = if dev.muted { " [MUTED]" } else { "" };
        let vol_pct = (dev.volume * 100.0).round() as u32;

        let name_col = if sel { txt() } else { dim() };
        let vol_col = if dev.muted { dng() } else if sel { txt().add_modifier(Modifier::BOLD) } else { txt() };
        let mute_col = if dev.muted { dng() } else { g() };

        lines.push(Line::from(vec![
            Span::styled(prefix, if sel { g() } else { fnt() }),
            Span::styled(default_mark, g()),
            Span::styled(format!("{:<30}", dev.display_name), name_col),
            Span::styled(format!("{:>4}%", vol_pct), vol_col),
            Span::styled(mute_label, mute_col),
        ]));
        row += 1;

        // Port rows
        for port in &dev.ports {
            let sel = focused && row == cursor;
            let marker = if port.is_active { "  * " } else { "    " };
            let prefix = if sel { "> " } else { "  " };

            let port_col = if port.is_active {
                g()
            } else if sel {
                txt().add_modifier(Modifier::BOLD)
            } else if port.available {
                txt()
            } else {
                fnt()
            };
            let status = if port.is_active {
                "  (active)"
            } else if !port.available {
                "  (unavailable)"
            } else {
                ""
            };

            lines.push(Line::from(vec![
                Span::styled(prefix, if sel { g() } else { fnt() }),
                Span::styled(marker, port_col),
                Span::styled(&port.display_name, port_col),
                Span::styled(status, fnt()),
            ]));
            row += 1;
        }
    }

    lines
}

// ── Display tab ───────────────────────────────────────────────────────────────

fn render_display(f: &mut Frame, app: &TuiApp, area: Rect) {
    let focused = app.focus == Focus::Content;

    let rows = Layout::vertical([
        Constraint::Length(5),  // monitor info
        Constraint::Fill(1),    // mode list
    ]).split(area);

    let status_active = !matches!(app.status, Status::None);

    // ── Monitor info block ──────────────────────────────────────────────────────
    {
        let bdr       = if focused { Color::Rgb(35, 60, 45) } else { LINE };
        let title_col = if focused { GREEN } else { DIM };

        let block = Block::default()
            .title(Span::styled(" display ", Style::default().fg(title_col)))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(bdr));
        let inner = block.inner(rows[0]);
        f.render_widget(block, rows[0]);

        if !app.display.available {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  Display config not available  ", dng()),
                    Span::styled("requires GNOME/Mutter (Wayland)", dim()),
                ])),
                inner,
            );
            return;
        }

        let monitor = match app.display.current_monitor() {
            Some(m) => m,
            None => return,
        };

        let current_mode = monitor.modes.iter().find(|m| m.is_current);
        let res_str = current_mode
            .map(|m| format!("{}x{}", m.width, m.height))
            .unwrap_or_else(|| "unknown".to_string());
        let current_rate = current_mode.map(|m| m.refresh_rate).unwrap_or(0.0);

        let cells = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
        ]).split(inner);

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  monitor  ", fnt()),
                Span::styled(&monitor.display_name, dim()),
                Span::styled(format!("  ({})", monitor.connector), fnt()),
            ])),
            cells[0],
        );

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  current  ", fnt()),
                Span::styled(format!("{res_str}  "), txt()),
                Span::styled(format!("{current_rate:.0}Hz"), g()),
            ])),
            cells[1],
        );
    }

    // ── Mode list ──────────────────────────────────────────────────────────────
    {
        let bdr = if focused { Color::Rgb(35, 35, 35) } else { LINE };
        let block = Block::default()
            .title(Span::styled(" modes (j/k navigate, a/enter to apply) ", Style::default().fg(if focused { DIM } else { FAINT })))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(bdr));
        let inner = block.inner(rows[1]);
        f.render_widget(block, rows[1]);

        let modes = app.display.current_monitor_modes();
        if modes.is_empty() {
            f.render_widget(Paragraph::new(Span::styled("  no modes available", fnt())), inner);
            return;
        }

        let mut lines: Vec<Line> = vec![];
        for (i, mode) in modes.iter().enumerate() {
            let sel = focused && i == app.display.pending_mode_idx;
            let cursor = if sel { "▶ " } else { "  " };
            let is_current = mode.is_current;
            let is_pending = i == app.display.pending_mode_idx;

            let label = format!("{}x{}", mode.width, mode.height);
            let rate_str = format!("{:.0}Hz", mode.refresh_rate);

            let res_style = match (is_current, is_pending) {
                (true, true) => bg(),
                (false, true) => txt().add_modifier(Modifier::BOLD),
                (true, false) => g(),
                (false, false) => dim(),
            };
            let rate_style = match (is_current, is_pending) {
                (true, true) => bg(),
                (false, true) => txt().add_modifier(Modifier::BOLD),
                (true, false) => g(),
                (false, false) => txt(),
            };
            let marker = if is_current { " *" } else { "  " };

            lines.push(Line::from(vec![
                Span::styled(cursor, if sel { g() } else { fnt() }),
                Span::styled(format!("{:<14}", label), res_style),
                Span::styled(format!("{:>7}", rate_str), rate_style),
                Span::styled(marker, g()),
            ]));
        }

        if status_active {
            lines.push(Line::from(""));
            match &app.status {
                Status::Ok(msg) => lines.push(Line::from(vec![Span::styled(format!("  ✓  {msg}"), g())])),
                Status::Err(msg) => lines.push(Line::from(vec![Span::styled(format!("  ✗  {msg}"), dng())])),
                Status::None => {}
            }
        }

        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }
}

// ── Settings tab ──────────────────────────────────────────────────────────────

fn render_settings(f: &mut Frame, app: &TuiApp, area: Rect) {
    let focused = app.focus == Focus::Content;

    let block = Block::default()
        .title(Span::styled(" settings ", fnt()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused { Color::Rgb(35, 35, 35) } else { LINE }));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Length(1), // spacer
        Constraint::Fill(1),   // settings list
    ]).split(inner);

    // Column header
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("  {:<32}", "setting"), fnt()),
            Span::styled(format!("{:<16}", "value"), fnt()),
            Span::styled("key", fnt()),
        ])),
        rows[0],
    );

    let boost_val = match app.config.boost_enabled {
        Some(true) => "on".to_string(),
        Some(false) => "off".to_string(),
        None => "unset".to_string(),
    };

    let settings: &[(&str, String, &str)] = &[
        ("default charge limit",  format!("{}%",  app.config.charge_limit),           "← →"),
        ("apply on startup",      (if app.config.auto_apply_on_start {"on"} else {"off"}).to_string(), "space/↵"),
        ("refresh interval",      format!("{}s",  app.config.refresh_secs),            "← →"),
        ("persist cpu boost",     boost_val,                                            "space/↵"),
        ("persistence setup",     (if app.battery.info.persistent_enabled {"configured"} else {"not configured"}).to_string(), "↵"),
    ];

    let mut lines: Vec<Line> = vec![];
    for (i, (label, value, key)) in settings.iter().enumerate() {
        let sel = focused && i == app.settings_cursor;
        let cursor = if sel { "▶ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(cursor,                     if sel { g() } else { fnt() }),
            Span::styled(format!("{:<30}", label),   if sel { txt() } else { dim() }),
            Span::styled(format!("{:<16}", value),   if sel { bg() } else { txt() }),
            Span::styled(format!("[{key}]"),          fnt()),
        ]));
        lines.push(Line::from("")); // breathing room between rows
    }

    // Status at bottom
    match &app.status {
        Status::Ok(msg)  => lines.push(Line::from(vec![Span::styled(format!("  ✓  {msg}"), g())])),
        Status::Err(msg) => lines.push(Line::from(vec![Span::styled(format!("  ✗  {msg}"), dng())])),
        Status::None     => {}
    }

    f.render_widget(Paragraph::new(Text::from(lines)), rows[2]);
}

// ── Status bar (keybindings) ──────────────────────────────────────────────────

fn render_statusbar(f: &mut Frame, app: &TuiApp, area: Rect) {
    let keys = match (app.active_tab, app.focus) {
        (0, Focus::Content) => "  q quit   tab→sidebar   r refresh",
        (1, Focus::Content) => "  q quit   tab→sidebar   ←→ limit   shift+←→ ±5   1/2/3 presets   a/↵ apply   s setup/persist   r refresh",
        (2, Focus::Content) => "  q quit   tab→sidebar   r refresh",
        (3, Focus::Content) => "  q quit   tab→sidebar   j/k navigate   ←→ adjust   space toggle   a/↵ apply   r refresh",
        (4, Focus::Content) => "  q quit   tab→sidebar   ← → select mode   a/↵ apply   r refresh",
        (5, Focus::Content) => "  q quit   tab output/input   j/k navigate   enter activate port   space mute   d default",
        (6, Focus::Content) => "  q quit   tab→sidebar   j/k navigate   a/↵ apply",
        (7, Focus::Content) => "  q quit   tab→sidebar   j/k navigate   ←→ adjust   space/↵ toggle   r refresh",
        (_, Focus::Sidebar)  => "  q quit   j/k navigate   ↵/→ select tab   tab→content   r refresh",
        _                    => "  q quit",
    };
    f.render_widget(
        Paragraph::new(Span::styled(keys, fnt()))
            .style(Style::default().bg(Color::Rgb(10, 10, 10))),
        area,
    );
}
