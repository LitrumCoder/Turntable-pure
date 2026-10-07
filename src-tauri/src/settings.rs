use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::widget::SKINS;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub skin: String,
    pub docked: bool,
    pub x: i32,
    pub y: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            skin: SKINS[0].0.to_string(),
            docked: true,
            x: 0,
            y: 0,
        }
    }
}

pub struct SettingsState(Mutex<Settings>);

impl SettingsState {
    pub fn load(app: &AppHandle) -> Self {
        let mut settings: Settings = file_path(app)
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if !SKINS.iter().any(|(id, _)| *id == settings.skin) {
            settings.skin = Settings::default().skin;
        }
        Self(Mutex::new(settings))
    }

    fn lock(&self) -> MutexGuard<'_, Settings> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn file_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
}

pub fn get(app: &AppHandle) -> Settings {
    app.state::<SettingsState>().lock().clone()
}

pub fn update(app: &AppHandle, change: impl FnOnce(&mut Settings)) {
    let state = app.state::<SettingsState>();
    let mut settings = state.lock();
    change(&mut settings);

    let Some(path) = file_path(app) else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&*settings) {
        if let Err(e) = fs::write(&path, json) {
            eprintln!("не удалось сохранить настройки: {e}");
        }
    }
}
