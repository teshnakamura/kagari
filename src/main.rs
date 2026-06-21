//! kagari (篝) — a Wayland-native live system metrics graph.
//!
//! Named after the kagari-bi, a watchfire kept burning through the night to keep
//! watch: it watches (monitoring), it is about heat, and it never goes dark.
//!
//! psensor (a GTK2 app running under XWayland) suffers from Mutter's frame-callback
//! throttling: the graph stops updating even while the window is visible. This tool
//! avoids that by design, cleanly separating two concerns:
//!   - Collection timer (glib::timeout): always polls sensors / proc / nvidia-smi and
//!     appends to per-metric history, independently of drawing.
//!   - Drawing (DrawingArea draw_func): runs only while the window is visible.
//!
//! Metrics with different units (temperature, usage %, fan RPM) are drawn in separate
//! horizontally stacked bands, each with its own auto-scaled Y axis. Every series can
//! be toggled on/off from the side panel.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::process::Command;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::cairo::{FontSlant, FontWeight};
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, CheckButton, DrawingArea, Label, Orientation,
    PolicyType, ScrolledWindow,
};

const APP_ID: &str = "info.teshnakamura.Kagari";
const POLL_INTERVAL_SECS: u32 = 2;
/// Number of history points. At POLL_INTERVAL_SECS spacing, 600 points = 20 minutes.
const HISTORY_LEN: usize = 600;
const WINDOW_W: i32 = 1100;
const WINDOW_H: i32 = 620;
const SIDE_PANEL_W: i32 = 200;

const MARGIN: f64 = 10.0;
const AXIS_LEFT: f64 = 42.0; // space for Y tick labels
const VALUE_RIGHT: f64 = 58.0; // space for latest-value labels
const BAND_GAP: f64 = 10.0;
const BAND_TITLE_H: f64 = 16.0;
const Y_TICKS: usize = 4;
/// Minimum Y span (used by auto-scaled bands so a flat line does not collapse).
const MIN_Y_SPAN: f64 = 8.0;

fn debug_enabled() -> bool {
    std::env::var("KAGARI_DEBUG").map(|v| v == "1").unwrap_or(false)
}

/// Path to a config file under $XDG_CONFIG_HOME/kagari/ (falling back to
/// ~/.config/kagari/).
fn config_file(name: &str) -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("kagari").join(name))
}

/// Write `contents` to a config file atomically (temp file + rename). Failures
/// are ignored so the app keeps working even if the config dir is unwritable.
fn write_config(name: &str, contents: &str) {
    let Some(path) = config_file(name) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, contents).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn load_visibility() -> BTreeMap<String, bool> {
    config_file("visibility.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_visibility(map: &BTreeMap<String, bool>) {
    if let Ok(json) = serde_json::to_string_pretty(map) {
        write_config("visibility.json", &json);
    }
}

/// Restore the last window size, falling back to the defaults. Values are
/// clamped to a sane minimum so a corrupt config cannot produce a tiny window.
fn load_window_size() -> (i32, i32) {
    let def = (WINDOW_W, WINDOW_H);
    let value = config_file("window.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let Some(v) = value else { return def };
    let w = v.get("width").and_then(|x| x.as_i64()).unwrap_or(def.0 as i64) as i32;
    let h = v.get("height").and_then(|x| x.as_i64()).unwrap_or(def.1 as i64) as i32;
    (w.max(400), h.max(300))
}

fn save_window_size(width: i32, height: i32) {
    let json = serde_json::json!({ "width": width, "height": height }).to_string();
    write_config("window.json", &json);
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Unit {
    Celsius,
    Percent,
    Rpm,
}

impl Unit {
    fn title(self) -> &'static str {
        match self {
            Unit::Celsius => "Temperature (°C)",
            Unit::Percent => "Usage (%)",
            Unit::Rpm => "Fan (RPM)",
        }
    }
    fn suffix(self) -> &'static str {
        match self {
            Unit::Celsius => "°C",
            Unit::Percent => "%",
            Unit::Rpm => "rpm",
        }
    }
}

/// Fixed order in which bands are stacked top to bottom.
const BAND_ORDER: [Unit; 3] = [Unit::Celsius, Unit::Percent, Unit::Rpm];

struct Metric {
    unit: Unit,
    history: VecDeque<f64>,
    visible: bool,
    color_index: usize,
}

struct AppState {
    /// Keyed by label; BTreeMap keeps a stable order for legend/colors.
    metrics: BTreeMap<String, Metric>,
    /// Previous /proc/stat counters per CPU line: (idle_all, total).
    prev_cpu: HashMap<String, (u64, u64)>,
    poll_count: u64,
    next_color: usize,
    /// Persisted per-series on/off state (label -> visible), loaded at startup
    /// and written to disk whenever a toggle changes.
    visibility: BTreeMap<String, bool>,
}

/// 16-color palette; cycled if there are more series than colors.
const PALETTE: [(f64, f64, f64); 16] = [
    (0.90, 0.16, 0.22),
    (0.13, 0.45, 0.85),
    (0.18, 0.68, 0.35),
    (0.95, 0.61, 0.07),
    (0.61, 0.35, 0.71),
    (0.09, 0.64, 0.72),
    (0.85, 0.37, 0.61),
    (0.55, 0.55, 0.10),
    (0.40, 0.40, 0.45),
    (0.20, 0.30, 0.65),
    (0.80, 0.52, 0.25),
    (0.16, 0.70, 0.55),
    (0.70, 0.20, 0.20),
    (0.45, 0.62, 0.16),
    (0.30, 0.50, 0.90),
    (0.62, 0.18, 0.50),
];

fn color_for(index: usize) -> (f64, f64, f64) {
    PALETTE[index % PALETTE.len()]
}

/// "coretemp-isa-0000" -> "coretemp": drop the adapter id to keep labels short.
fn short_chip(chip: &str) -> &str {
    chip.split('-').next().unwrap_or(chip)
}

/// Collect one sample of every available metric. Failing sources are skipped silently.
fn collect(prev_cpu: &mut HashMap<String, (u64, u64)>) -> Vec<(String, Unit, f64)> {
    let mut out = Vec::new();

    // lm-sensors: temperatures (tempN_input) and fans (fanN_input).
    if let Ok(o) = Command::new("sensors").arg("-j").output() {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&o.stdout) {
            if let Some(chips) = v.as_object() {
                for (chip, feats) in chips {
                    let Some(feats) = feats.as_object() else { continue };
                    for (feat, vals) in feats {
                        let Some(vals) = vals.as_object() else { continue };
                        for (key, val) in vals {
                            let Some(num) = val.as_f64() else { continue };
                            if key.starts_with("temp") && key.ends_with("_input") {
                                out.push((format!("{} {}", short_chip(chip), feat), Unit::Celsius, num));
                            } else if key.starts_with("fan") && key.ends_with("_input") {
                                out.push((format!("{} {}", short_chip(chip), feat), Unit::Rpm, num));
                            }
                        }
                    }
                }
            }
        }
    }

    // NVIDIA GPU: temperature (°C) and fan speed (percent; RPM is not exposed).
    if let Ok(o) = Command::new("nvidia-smi")
        .args(["--query-gpu=temperature.gpu,fan.speed", "--format=csv,noheader,nounits"])
        .output()
    {
        if let Ok(s) = String::from_utf8(o.stdout) {
            if let Some(line) = s.lines().next() {
                let parts: Vec<&str> = line.split(',').map(|p| p.trim()).collect();
                if let Some(t) = parts.first().and_then(|p| p.parse::<f64>().ok()) {
                    out.push(("nvidia GPU".to_string(), Unit::Celsius, t));
                }
                if let Some(f) = parts.get(1).and_then(|p| p.parse::<f64>().ok()) {
                    out.push(("GPU fan".to_string(), Unit::Percent, f));
                }
            }
        }
    }

    // CPU usage (overall + per core) from /proc/stat deltas.
    if let Ok(stat) = fs::read_to_string("/proc/stat") {
        for line in stat.lines() {
            if !line.starts_with("cpu") {
                break; // cpu lines are first and contiguous
            }
            let mut it = line.split_whitespace();
            let Some(name) = it.next() else { continue };
            let nums: Vec<u64> = it.filter_map(|t| t.parse::<u64>().ok()).collect();
            if nums.len() < 5 {
                continue;
            }
            let idle_all = nums[3] + nums[4]; // idle + iowait
            let total: u64 = nums.iter().sum();
            let label = if name == "cpu" {
                "CPU total".to_string()
            } else {
                format!("CPU core {}", &name[3..])
            };
            let pct = match prev_cpu.get(name) {
                Some(&(prev_idle, prev_total)) if total > prev_total => {
                    let d_total = (total - prev_total) as f64;
                    let d_idle = (idle_all.saturating_sub(prev_idle)) as f64;
                    (1.0 - d_idle / d_total) * 100.0
                }
                _ => 0.0,
            };
            prev_cpu.insert(name.to_string(), (idle_all, total));
            out.push((label, Unit::Percent, pct.clamp(0.0, 100.0)));
        }
    }

    // Memory usage from /proc/meminfo.
    if let Ok(mem) = fs::read_to_string("/proc/meminfo") {
        let mut total = 0.0;
        let mut avail = 0.0;
        for line in mem.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                total = parse_kb(rest);
            } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                avail = parse_kb(rest);
            }
        }
        if total > 0.0 {
            out.push(("Memory".to_string(), Unit::Percent, (total - avail) / total * 100.0));
        }
    }

    out
}

fn parse_kb(s: &str) -> f64 {
    s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0)
}

/// Ensure a metric and its side-panel toggle row exist before appending a value.
fn ensure_metric(state: &Rc<RefCell<AppState>>, list: &GtkBox, label: &str, unit: Unit) {
    let color;
    let visible;
    {
        let mut st = state.borrow_mut();
        if st.metrics.contains_key(label) {
            return;
        }
        let ci = st.next_color;
        st.next_color += 1;
        color = color_for(ci);
        // Restore the saved on/off state for this series (default on).
        visible = st.visibility.get(label).copied().unwrap_or(true);
        st.metrics.insert(
            label.to_string(),
            Metric { unit, history: VecDeque::new(), visible, color_index: ci },
        );
    }

    // Build the toggle row: color swatch + checkbox.
    let row = GtkBox::new(Orientation::Horizontal, 6);
    row.set_margin_start(8);
    row.set_margin_end(8);

    let swatch = DrawingArea::new();
    swatch.set_content_width(14);
    swatch.set_content_height(14);
    swatch.set_valign(gtk::Align::Center);
    swatch.set_draw_func(move |_, cr, w, h| {
        cr.set_source_rgb(color.0, color.1, color.2);
        cr.rectangle(0.0, 0.0, w as f64, h as f64);
        let _ = cr.fill();
    });

    let check = CheckButton::with_label(label);
    check.set_active(visible);
    {
        let state = state.clone();
        let label = label.to_string();
        check.connect_toggled(move |c| {
            let active = c.is_active();
            let mut st = state.borrow_mut();
            if let Some(m) = st.metrics.get_mut(&label) {
                m.visible = active;
            }
            st.visibility.insert(label.clone(), active);
            save_visibility(&st.visibility);
        });
    }

    row.append(&swatch);
    row.append(&check);
    list.append(&row);
}

fn build_ui(app: &Application) {
    let state = Rc::new(RefCell::new(AppState {
        metrics: BTreeMap::new(),
        prev_cpu: HashMap::new(),
        poll_count: 0,
        next_color: 0,
        visibility: load_visibility(),
    }));

    // Side panel: scrollable list of per-series toggles.
    let list = GtkBox::new(Orientation::Vertical, 2);
    list.set_margin_top(6);
    list.set_margin_bottom(6);
    let header = Label::new(Some("Sensors"));
    header.set_margin_start(8);
    header.set_halign(gtk::Align::Start);
    list.append(&header);

    let scroller = ScrolledWindow::new();
    scroller.set_policy(PolicyType::Never, PolicyType::Automatic);
    scroller.set_min_content_width(SIDE_PANEL_W);
    scroller.set_child(Some(&list));

    // Graph area.
    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let state = state.clone();
        area.set_draw_func(move |_area, cr, w, h| {
            draw_graph(cr, w, h, &state.borrow());
        });
    }

    // Prime once so the graph has data immediately, and seed prev_cpu.
    {
        let samples = collect(&mut state.borrow_mut().prev_cpu);
        for (label, unit, value) in samples {
            ensure_metric(&state, &list, &label, unit);
            if let Some(m) = state.borrow_mut().metrics.get_mut(&label) {
                m.history.push_back(value);
            }
        }
        // Persist the (possibly newly discovered) set so the file always exists.
        let mut st = state.borrow_mut();
        st.poll_count += 1;
        let entries: Vec<(String, bool)> =
            st.metrics.iter().map(|(l, m)| (l.clone(), m.visible)).collect();
        for (label, vis) in entries {
            st.visibility.entry(label).or_insert(vis);
        }
        let snapshot = st.visibility.clone();
        drop(st);
        save_visibility(&snapshot);
    }

    // Collection timer: runs independently of drawing.
    {
        let state = state.clone();
        let list = list.clone();
        let area = area.clone();
        glib::timeout_add_seconds_local(POLL_INTERVAL_SECS, move || {
            let samples = collect(&mut state.borrow_mut().prev_cpu);
            for (label, unit, value) in samples {
                ensure_metric(&state, &list, &label, unit);
                let mut st = state.borrow_mut();
                if let Some(m) = st.metrics.get_mut(&label) {
                    m.history.push_back(value);
                    while m.history.len() > HISTORY_LEN {
                        m.history.pop_front();
                    }
                }
            }
            let mut st = state.borrow_mut();
            st.poll_count += 1;
            if debug_enabled() {
                let visible = st.metrics.values().filter(|m| m.visible).count();
                eprintln!(
                    "[poll #{}] metrics={} visible={}",
                    st.poll_count,
                    st.metrics.len(),
                    visible
                );
            }
            drop(st);
            area.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    let content = GtkBox::new(Orientation::Horizontal, 0);
    content.append(&scroller);
    content.append(&area);

    let (win_w, win_h) = load_window_size();
    let window = ApplicationWindow::builder()
        .application(app)
        .title("kagari")
        .default_width(win_w)
        .default_height(win_h)
        .child(&content)
        .build();

    // Persist the window size on close. In GTK4 the default size tracks the
    // current (non-maximized) size, so this captures the user's last resize.
    window.connect_close_request(move |w| {
        let (width, height) = w.default_size();
        save_window_size(width, height);
        glib::Propagation::Proceed
    });

    window.present();
}

fn draw_graph(cr: &gtk::cairo::Context, w: i32, h: i32, st: &AppState) {
    let w = w as f64;
    let h = h as f64;

    cr.set_source_rgb(0.97, 0.97, 0.95);
    let _ = cr.paint();

    cr.select_font_face("Sans", FontSlant::Normal, FontWeight::Normal);

    let plot_left = MARGIN + AXIS_LEFT;
    let plot_right = w - VALUE_RIGHT;
    let plot_w = (plot_right - plot_left).max(1.0);

    // Determine which bands have at least one visible series.
    let active: Vec<Unit> = BAND_ORDER
        .iter()
        .copied()
        .filter(|u| st.metrics.values().any(|m| m.visible && m.unit == *u))
        .collect();

    if active.is_empty() {
        cr.set_source_rgb(0.4, 0.4, 0.4);
        cr.set_font_size(13.0);
        cr.move_to(plot_left, h / 2.0);
        let _ = cr.show_text("No series selected");
        return;
    }

    let area_top = MARGIN;
    let area_bottom = h - MARGIN - 14.0; // bottom caption space
    let n = active.len() as f64;
    let band_h = ((area_bottom - area_top) - BAND_GAP * (n - 1.0)) / n;

    let x_of = |idx_from_right: usize| {
        let dx = plot_w / (HISTORY_LEN as f64 - 1.0);
        plot_right - idx_from_right as f64 * dx
    };

    for (bi, unit) in active.iter().enumerate() {
        let band_top = area_top + bi as f64 * (band_h + BAND_GAP);
        let band_bottom = band_top + band_h;
        let plot_top = band_top + BAND_TITLE_H;

        // Y range for this band.
        let (mut ymin, mut ymax) = if *unit == Unit::Percent {
            (0.0, 100.0)
        } else {
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for m in st.metrics.values().filter(|m| m.visible && m.unit == *unit) {
                for &v in &m.history {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            if !lo.is_finite() || !hi.is_finite() {
                (0.0, 1.0)
            } else if *unit == Unit::Rpm {
                (0.0, hi.max(1.0))
            } else {
                (lo, hi)
            }
        };
        if ymax - ymin < MIN_Y_SPAN {
            ymax = ymin + MIN_Y_SPAN;
        }
        if *unit == Unit::Celsius {
            let pad = (ymax - ymin) * 0.05;
            ymin -= pad;
            ymax += pad;
        }
        let yspan = ymax - ymin;
        let y_of = |v: f64| band_bottom - (v - ymin) / yspan * (band_bottom - plot_top);

        // Band title.
        cr.set_source_rgb(0.25, 0.25, 0.25);
        cr.set_font_size(11.0);
        cr.move_to(plot_left, band_top + 11.0);
        let _ = cr.show_text(unit.title());

        // Grid + Y tick labels.
        cr.set_font_size(10.0);
        for i in 0..=Y_TICKS {
            let frac = i as f64 / Y_TICKS as f64;
            let v = ymin + yspan * frac;
            let y = y_of(v);
            cr.set_source_rgb(0.86, 0.86, 0.84);
            cr.set_line_width(1.0);
            cr.move_to(plot_left, y);
            cr.line_to(plot_right, y);
            let _ = cr.stroke();
            cr.set_source_rgb(0.4, 0.4, 0.4);
            cr.move_to(MARGIN, y + 3.5);
            let _ = cr.show_text(&format!("{:.0}", v));
        }

        // Frame.
        cr.set_source_rgb(0.6, 0.6, 0.58);
        cr.set_line_width(1.0);
        cr.rectangle(plot_left, plot_top, plot_w, band_bottom - plot_top);
        let _ = cr.stroke();

        // Series lines + latest-value labels.
        for m in st.metrics.values().filter(|m| m.visible && m.unit == *unit) {
            let (r, g, b) = color_for(m.color_index);
            if m.history.len() >= 2 {
                cr.set_source_rgb(r, g, b);
                cr.set_line_width(1.6);
                let len = m.history.len();
                for (i, &v) in m.history.iter().enumerate() {
                    let from_right = len - 1 - i;
                    let x = x_of(from_right);
                    let y = y_of(v.clamp(ymin, ymax));
                    if i == 0 {
                        cr.move_to(x, y);
                    } else {
                        cr.line_to(x, y);
                    }
                }
                let _ = cr.stroke();
            }
            if let Some(&latest) = m.history.back() {
                cr.set_source_rgb(r, g, b);
                cr.set_font_size(10.0);
                cr.move_to(plot_right + 3.0, y_of(latest.clamp(ymin, ymax)) + 3.0);
                let _ = cr.show_text(&format!("{:.0}{}", latest, unit.suffix()));
            }
        }
    }

    // Bottom caption.
    cr.set_source_rgb(0.45, 0.45, 0.45);
    cr.set_font_size(10.0);
    cr.move_to(plot_left, h - 3.0);
    let _ = cr.show_text(&format!(
        "{}s interval / {} min history / poll {}",
        POLL_INTERVAL_SECS,
        HISTORY_LEN * POLL_INTERVAL_SECS as usize / 60,
        st.poll_count
    ));
}

fn main() -> glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}
