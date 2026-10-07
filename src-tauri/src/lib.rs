mod artwork;
mod media;
mod settings;
mod widget;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, Window, Wry};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

struct TrayChecks {
    skins: Vec<(&'static str, CheckMenuItem<Wry>)>,
    autostart: CheckMenuItem<Wry>,
}

struct AppMenu(Menu<Wry>);

#[tauri::command]
fn show_menu(window: Window, menu: State<AppMenu>) {
    if let Err(e) = window.popup_menu(&menu.0) {
        eprintln!("меню: {e}");
    }
}

fn select_skin(app: &AppHandle, skin: &str) {
    widget::set_skin(app, skin);
    let current = settings::get(app).skin;
    for (id, item) in &app.state::<TrayChecks>().skins {
        let _ = item.set_checked(*id == current);
    }
}

fn toggle_autostart(app: &AppHandle) {
    let launcher = app.autolaunch();
    let enabled = launcher.is_enabled().unwrap_or(false);
    let result = if enabled { launcher.disable() } else { launcher.enable() };
    if let Err(e) = result {
        eprintln!("автозапуск: {e}");
    }
    let _ = app
        .state::<TrayChecks>()
        .autostart
        .set_checked(launcher.is_enabled().unwrap_or(false));
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "toggle" => widget::toggle(app),
        "dock" => widget::dock(app),
        "autostart" => toggle_autostart(app),
        "quit" => app.exit(0),
        _ => {
            if let Some(skin) = id.strip_prefix("skin:") {
                select_skin(app, skin);
            } else if let Some(view) = id.strip_prefix("view:") {
                let _ = app.emit("view", view);
            }
        }
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let current = settings::get(app.handle()).skin;
    let skins = widget::SKINS
        .iter()
        .map(|&(id, name)| {
            CheckMenuItem::with_id(app, format!("skin:{id}"), name, true, id == current, None::<&str>)
                .map(|item| (id, item))
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    let skin_menu = Submenu::with_id(app, "skins", "Скин", true)?;
    for (_, item) in &skins {
        skin_menu.append(item)?;
    }

    let view_menu = Submenu::with_items(
        app,
        "Вид",
        true,
        &[
            &MenuItem::with_id(app, "view:top", "Сверху", true, None::<&str>)?,
            &MenuItem::with_id(app, "view:perspective", "В перспективе", true, None::<&str>)?,
            &MenuItem::with_id(app, "view:angle", "Под углом", true, None::<&str>)?,
            &MenuItem::with_id(app, "view:front", "Спереди", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "view:hint", "Колесо — наклон, Shift+колесо — поворот", false, None::<&str>)?,
        ],
    )?;

    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(app, "autostart", "Запускать вместе с Windows", true, autostart_on, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "toggle", "Показать / скрыть", true, None::<&str>)?,
            &MenuItem::with_id(app, "dock", "Вернуть в угол", true, None::<&str>)?,
            &skin_menu,
            &view_menu,
            &autostart,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?,
        ],
    )?;
    app.manage(TrayChecks { skins, autostart });
    app.manage(AppMenu(menu.clone()));

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Vinyl")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                widget::toggle(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| widget::show(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .manage(media::CoverState::default())
        .manage(widget::Visibility::default())
        .manage(widget::HitRegion::default())
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(settings::SettingsState::load(&handle));
            build_tray(app)?;
            widget::apply_layout(&handle)?;
            if let Some(window) = app.get_webview_window("main") {
                window.show()?;
            }
            media::spawn_watcher(handle.clone());
            widget::spawn_fullscreen_watcher(handle.clone());
            widget::spawn_click_through(handle);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            media::current_cover,
            media::toggle_play,
            media::next_track,
            media::prev_track,
            widget::begin_drag,
            widget::current_skin,
            widget::set_hit_region,
            show_menu
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
