use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, Monitor, PhysicalPosition, PhysicalSize, State,
    WebviewWindow,
};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::settings;

pub const SKINS: [(&str, &str); 4] = [
    ("pioneer", "Pioneer — серебро и орех"),
    ("studio", "Studio — алюминий и орех"),
    ("braun", "Braun — минимализм"),
    ("dual", "Dual — тик"),
];

const EDGE_MARGIN: f64 = 8.0;
const ATTACH_RADIUS: f64 = 40.0;
const DETACH_RADIUS: f64 = 64.0;
const MAGNET_PULL: f64 = 0.25;
const FREE_SPRING: (f64, f64) = (1400.0, 1.0);
const MAGNET_SPRING: (f64, f64) = (260.0, 0.5);
const GLIDE_SPRING: (f64, f64) = (180.0, 0.72);
const DECK_INSET: f64 = 10.0;
const DECK_RADIUS: f64 = 7.0;
const HOVER_POLL: Duration = Duration::from_millis(40);
const PHYSICS_STEP: f64 = 0.004;
const FRAME: Duration = Duration::from_millis(7);
const FULLSCREEN_POLL: Duration = Duration::from_millis(1500);

static MOTION: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub struct HitRegion(Mutex<Vec<(f64, f64)>>);

#[tauri::command]
pub fn set_hit_region(region: State<HitRegion>, points: Vec<(f64, f64)>) {
    if let Ok(mut current) = region.0.lock() {
        *current = points;
    }
}

fn inside(polygon: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut hit = false;
    let mut j = polygon.len().wrapping_sub(1);
    for (i, &(xi, yi)) in polygon.iter().enumerate() {
        let (xj, yj) = polygon[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            hit = !hit;
        }
        j = i;
    }
    hit
}

#[derive(Default)]
pub struct Visibility {
    user_hidden: AtomicBool,
    auto_hidden: AtomicBool,
}

fn skin_size(skin: &str) -> LogicalSize<f64> {
    match skin {
        "braun" => LogicalSize::new(240.0, 240.0),
        "studio" => LogicalSize::new(300.0, 256.0),
        _ => LogicalSize::new(300.0, 270.0),
    }
}

fn main_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("main")
}

fn monitor_at(app: &AppHandle, x: i32, y: i32) -> Option<Monitor> {
    let inside = |m: &Monitor| {
        let (p, s) = (m.position(), m.size());
        x >= p.x && y >= p.y && x < p.x + s.width as i32 && y < p.y + s.height as i32
    };
    app.available_monitors().ok()?.into_iter().find(inside)
}

fn dock_point(monitor: &Monitor, size: PhysicalSize<u32>) -> PhysicalPosition<i32> {
    let area = monitor.work_area();
    let margin = (EDGE_MARGIN * monitor.scale_factor()).round() as i32;
    PhysicalPosition::new(
        area.position.x + area.size.width as i32 - size.width as i32 - margin,
        area.position.y + area.size.height as i32 - size.height as i32 - margin,
    )
}

fn clamp_to(monitor: &Monitor, size: PhysicalSize<u32>, pos: PhysicalPosition<i32>) -> PhysicalPosition<i32> {
    let area = monitor.work_area();
    let (left, top) = (area.position.x, area.position.y);
    let right = (left + area.size.width as i32 - size.width as i32).max(left);
    let bottom = (top + area.size.height as i32 - size.height as i32).max(top);
    PhysicalPosition::new(pos.x.clamp(left, right), pos.y.clamp(top, bottom))
}

fn primary_dock(window: &WebviewWindow) -> Option<PhysicalPosition<i32>> {
    let monitor = window.primary_monitor().ok()??;
    let size = window.outer_size().ok()?;
    Some(dock_point(&monitor, size))
}

pub fn apply_layout(app: &AppHandle) -> tauri::Result<()> {
    let Some(window) = main_window(app) else { return Ok(()) };
    let s = settings::get(app);
    let logical = skin_size(&s.skin);
    window.set_size(logical)?;

    let saved = (!s.docked).then(|| monitor_at(app, s.x, s.y)).flatten();
    let position = match saved {
        Some(m) => clamp_to(&m, logical.to_physical(m.scale_factor()), PhysicalPosition::new(s.x, s.y)),
        None => {
            let Some(m) = window.primary_monitor()? else { return Ok(()) };
            dock_point(&m, logical.to_physical(m.scale_factor()))
        }
    };
    MOTION.fetch_add(1, Ordering::Relaxed);
    window.set_position(position)
}

pub fn set_skin(app: &AppHandle, skin: &str) {
    if !SKINS.iter().any(|(id, _)| *id == skin) {
        return;
    }
    settings::update(app, |s| s.skin = skin.to_string());
    let _ = app.emit("skin", skin);
    let _ = apply_layout(app);
}

pub fn dock(app: &AppHandle) {
    let Some(window) = main_window(app) else { return };
    let (Ok(from), Some(to)) = (window.outer_position(), primary_dock(&window)) else { return };
    settings::update(app, |s| {
        s.docked = true;
        s.x = to.x;
        s.y = to.y;
    });
    thread::spawn(move || glide(window, from, to));
}

#[tauri::command]
pub fn current_skin(app: AppHandle) -> String {
    settings::get(&app).skin
}

#[derive(Clone, Copy)]
struct Spring {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
}

impl Spring {
    fn at(p: PhysicalPosition<i32>) -> Self {
        Self { x: p.x.into(), y: p.y.into(), vx: 0.0, vy: 0.0 }
    }

    fn advance(&mut self, target: (f64, f64), (stiffness, damping): (f64, f64), dt: f64) {
        let friction = 2.0 * damping * stiffness.sqrt();
        let mut left = dt;
        while left > 0.0 {
            let h = left.min(PHYSICS_STEP);
            self.vx += (stiffness * (target.0 - self.x) - friction * self.vx) * h;
            self.vy += (stiffness * (target.1 - self.y) - friction * self.vy) * h;
            self.x += self.vx * h;
            self.y += self.vy * h;
            left -= h;
        }
    }

    fn settled(&self, target: (f64, f64)) -> bool {
        (target.0 - self.x).abs() < 0.5 && (target.1 - self.y).abs() < 0.5 && self.vx.hypot(self.vy) < 20.0
    }

    fn position(&self) -> PhysicalPosition<i32> {
        PhysicalPosition::new(self.x.round() as i32, self.y.round() as i32)
    }
}

fn point(p: PhysicalPosition<i32>) -> (f64, f64) {
    (p.x.into(), p.y.into())
}

fn glide(window: WebviewWindow, from: PhysicalPosition<i32>, to: PhysicalPosition<i32>) {
    let motion = MOTION.fetch_add(1, Ordering::Relaxed) + 1;
    let target = point(to);
    let mut spring = Spring::at(from);
    let mut last = Instant::now();
    while MOTION.load(Ordering::Relaxed) == motion && !spring.settled(target) {
        let now = Instant::now();
        spring.advance(target, GLIDE_SPRING, (now - last).as_secs_f64().min(0.05));
        last = now;
        let _ = window.set_position(spring.position());
        thread::sleep(FRAME);
    }
    if MOTION.load(Ordering::Relaxed) == motion {
        let _ = window.set_position(to);
    }
}

fn cursor() -> Option<POINT> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some(p)
}

fn left_button_down() -> bool {
    let state = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) };
    state < 0
}

#[tauri::command]
pub fn begin_drag(app: AppHandle, grab_x: f64, grab_y: f64) {
    thread::spawn(move || {
        let Some(window) = main_window(&app) else { return };
        let (Ok(start), Ok(size), Ok(scale)) = (window.outer_position(), window.outer_size(), window.scale_factor()) else {
            return;
        };
        let motion = MOTION.fetch_add(1, Ordering::Relaxed) + 1;
        let grab = ((grab_x * scale).round() as i32, (grab_y * scale).round() as i32);

        let mut spring = Spring::at(start);
        let mut free = point(start);
        let mut magnet = settings::get(&app).docked;
        let mut dock = None;
        let mut held = true;
        let mut shown = start;
        let mut last = Instant::now();

        while MOTION.load(Ordering::Relaxed) == motion {
            held = held && left_button_down();
            if held {
                if let Some(at) = cursor() {
                    free = (f64::from(at.x - grab.0), f64::from(at.y - grab.1));
                    dock = monitor_at(&app, at.x, at.y).map(|m| (point(dock_point(&m, size)), m.scale_factor()));
                }
            }

            let target = match dock {
                Some((d, monitor_scale)) => {
                    let distance = (free.0 - d.0).hypot(free.1 - d.1);
                    if held {
                        let radius = if magnet { DETACH_RADIUS } else { ATTACH_RADIUS };
                        magnet = distance <= radius * monitor_scale;
                    }
                    match (magnet, held) {
                        (true, true) => (d.0 + (free.0 - d.0) * MAGNET_PULL, d.1 + (free.1 - d.1) * MAGNET_PULL),
                        (true, false) => d,
                        _ => free,
                    }
                }
                None => free,
            };

            let now = Instant::now();
            let params = if magnet { MAGNET_SPRING } else { FREE_SPRING };
            spring.advance(target, params, (now - last).as_secs_f64().min(0.05));
            last = now;

            let pos = spring.position();
            if pos != shown {
                let _ = window.set_position(pos);
                shown = pos;
            }

            if !held && spring.settled(target) {
                let end = PhysicalPosition::new(target.0.round() as i32, target.1.round() as i32);
                if end != shown {
                    let _ = window.set_position(end);
                }
                if end != start {
                    settings::update(&app, |s| {
                        s.docked = magnet;
                        s.x = end.x;
                        s.y = end.y;
                    });
                }
                break;
            }
            thread::sleep(FRAME);
        }
    });
}

fn over_deck(window: &WebviewWindow, at: POINT) -> Option<bool> {
    let pos = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    let scale = window.scale_factor().ok()?;
    let inset = DECK_INSET * scale;
    let radius = DECK_RADIUS * scale;
    let x = f64::from(at.x - pos.x);
    let y = f64::from(at.y - pos.y);
    let (w, h) = (f64::from(size.width), f64::from(size.height));
    if x < 0.0 || y < 0.0 || x >= w || y >= h {
        return None;
    }

    let region = window.state::<HitRegion>();
    let polygon = region.0.lock().ok()?;
    if polygon.len() >= 3 {
        return Some(inside(&polygon, x / scale, y / scale));
    }

    let cx = x.clamp(inset + radius, w - inset - radius);
    let cy = y.clamp(inset + radius, h - inset - radius);
    Some((x - cx).hypot(y - cy) <= radius)
}

pub fn spawn_click_through(app: AppHandle) {
    thread::spawn(move || {
        let mut ignoring = false;
        loop {
            thread::sleep(HOVER_POLL);
            let Some(window) = main_window(&app) else { continue };
            let Some(inside) = cursor().and_then(|at| over_deck(&window, at)) else { continue };
            if inside == ignoring && window.set_ignore_cursor_events(!inside).is_ok() {
                ignoring = !inside;
            }
        }
    });
}

pub fn toggle(app: &AppHandle) {
    let Some(window) = main_window(app) else { return };
    if window.is_visible().unwrap_or(false) {
        hide(app);
    } else {
        show(app);
    }
}

pub fn show(app: &AppHandle) {
    let Some(window) = main_window(app) else { return };
    let visibility = app.state::<Visibility>();
    visibility.user_hidden.store(false, Ordering::Relaxed);
    visibility.auto_hidden.store(false, Ordering::Relaxed);
    let _ = window.show();
}

fn hide(app: &AppHandle) {
    let Some(window) = main_window(app) else { return };
    app.state::<Visibility>().user_hidden.store(true, Ordering::Relaxed);
    let _ = window.hide();
}

fn fullscreen_app_active() -> bool {
    let Ok(state) = (unsafe { SHQueryUserNotificationState() }) else {
        return false;
    };
    state == QUNS_BUSY || state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE
}

pub fn spawn_fullscreen_watcher(app: AppHandle) {
    thread::spawn(move || loop {
        thread::sleep(FULLSCREEN_POLL);
        let Some(window) = main_window(&app) else { continue };
        let visibility = app.state::<Visibility>();

        if fullscreen_app_active() {
            if window.is_visible().unwrap_or(false) {
                visibility.auto_hidden.store(true, Ordering::Relaxed);
                let _ = window.hide();
            }
        } else if visibility.auto_hidden.swap(false, Ordering::Relaxed)
            && !visibility.user_hidden.load(Ordering::Relaxed)
        {
            let _ = window.show();
        }
    });
}
