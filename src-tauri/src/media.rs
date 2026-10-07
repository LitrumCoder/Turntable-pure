use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use windows::core::{Result, HSTRING};
use windows::Foundation::{EventRegistrationToken, TypedEventHandler};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::{Buffer, DataReader, InputStreamOptions};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::artwork;

const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
const COVER_RETRIES: u8 = 3;
const FALLBACK_POLL: Duration = Duration::from_secs(5);
const RETRY_POLL: Duration = Duration::from_secs(1);
const EVENT_SETTLE: Duration = Duration::from_millis(60);

#[derive(Serialize, Clone, Debug)]
pub struct Track {
    title: String,
    artist: String,
    album: String,
    playing: bool,
    position: f64,
    duration: Option<f64>,
    source: String,
}

impl Track {
    fn key(&self) -> String {
        format!("{}\u{1f}{}\u{1f}{}", self.source, self.artist, self.title)
    }
}

#[derive(Default)]
struct CoverSlot {
    key: String,
    url: Option<String>,
    hq: bool,
}

#[derive(Default)]
pub struct CoverState(Mutex<CoverSlot>);

impl CoverState {
    fn lock(&self) -> MutexGuard<'_, CoverSlot> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn start_track(&self, key: &str) {
        let mut slot = self.lock();
        slot.key = key.to_string();
        slot.hq = false;
    }

    fn offer_thumbnail(&self, key: &str, url: Option<String>) -> bool {
        let mut slot = self.lock();
        if slot.key != key || slot.hq || slot.url == url {
            return false;
        }
        slot.url = url;
        true
    }

    fn offer_artwork(&self, key: &str, url: String) -> bool {
        let mut slot = self.lock();
        if slot.key != key {
            return false;
        }
        slot.url = Some(url);
        slot.hq = true;
        true
    }

    fn current(&self) -> Option<String> {
        self.lock().url.clone()
    }
}

#[tauri::command]
pub fn current_cover(state: State<CoverState>) -> Option<String> {
    state.current()
}

#[tauri::command]
pub fn toggle_play() {
    with_session(|s| s.TryTogglePlayPauseAsync()?.get().map(drop));
}

#[tauri::command]
pub fn next_track() {
    with_session(|s| s.TrySkipNextAsync()?.get().map(drop));
}

#[tauri::command]
pub fn prev_track() {
    with_session(|s| s.TrySkipPreviousAsync()?.get().map(drop));
}

fn init_winrt() {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

fn with_session(action: fn(&Session) -> Result<()>) {
    thread::spawn(move || {
        init_winrt();
        let result = SessionManager::RequestAsync()
            .and_then(|op| op.get())
            .and_then(|m| m.GetCurrentSession())
            .and_then(|s| action(&s));
        if let Err(e) = result {
            eprintln!("команда не выполнена: {e}");
        }
    });
}

fn notifier<S, A>(tx: &Sender<()>) -> impl FnMut(&Option<S>, &Option<A>) -> Result<()> + Send + 'static {
    let tx = tx.clone();
    move |_, _| {
        let _ = tx.send(());
        Ok(())
    }
}

struct Subscription {
    session: Session,
    source: HSTRING,
    media: Option<EventRegistrationToken>,
    playback: Option<EventRegistrationToken>,
    timeline: Option<EventRegistrationToken>,
}

impl Subscription {
    fn new(session: Session, tx: &Sender<()>) -> Self {
        Self {
            source: session.SourceAppUserModelId().unwrap_or_default(),
            media: session.MediaPropertiesChanged(&TypedEventHandler::new(notifier(tx))).ok(),
            playback: session.PlaybackInfoChanged(&TypedEventHandler::new(notifier(tx))).ok(),
            timeline: session.TimelinePropertiesChanged(&TypedEventHandler::new(notifier(tx))).ok(),
            session,
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(t) = self.media {
            let _ = self.session.RemoveMediaPropertiesChanged(t);
        }
        if let Some(t) = self.playback {
            let _ = self.session.RemovePlaybackInfoChanged(t);
        }
        if let Some(t) = self.timeline {
            let _ = self.session.RemoveTimelinePropertiesChanged(t);
        }
    }
}

fn follow_current(manager: &SessionManager, current: &mut Option<Subscription>, tx: &Sender<()>) {
    let session = manager.GetCurrentSession().ok();
    let source = session.as_ref().and_then(|s| s.SourceAppUserModelId().ok());
    if current.as_ref().map(|c| &c.source) != source.as_ref() {
        *current = session.map(|s| Subscription::new(s, tx));
    }
}

struct Watcher {
    app: AppHandle,
    last_key: String,
    last_playing: bool,
    cover_retries: u8,
    timeline: Option<Timeline>,
}

struct Timeline {
    key: String,
    position: f64,
    duration: f64,
    playing: bool,
    at: Instant,
}

impl Timeline {
    fn fill(slot: &mut Option<Timeline>, track: &mut Track, key: &str, now: Instant) {
        if let Some(duration) = track.duration {
            *slot = Some(Timeline {
                key: key.to_string(),
                position: track.position,
                duration,
                playing: track.playing,
                at: now,
            });
            return;
        }

        let Some(last) = slot.as_mut().filter(|t| t.key == key) else { return };
        if last.playing {
            let elapsed = now.saturating_duration_since(last.at).as_secs_f64();
            last.position = (last.position + elapsed).min(last.duration);
        }
        last.playing = track.playing;
        last.at = now;
        track.position = last.position;
        track.duration = Some(last.duration);
    }
}

impl Watcher {
    fn tick(&mut self, session: Option<&Session>) {
        let mut track = session.and_then(|s| read_track(s).ok());
        let key = track.as_ref().map(Track::key).unwrap_or_default();
        if let Some(t) = track.as_mut() {
            Timeline::fill(&mut self.timeline, t, &key, Instant::now());
        }
        let playing = track.as_ref().is_some_and(|t| t.playing);
        let covers = self.app.state::<CoverState>();

        if key != self.last_key {
            self.cover_retries = COVER_RETRIES;
            covers.start_track(&key);
            if let Some(t) = &track {
                fetch_artwork(self.app.clone(), key.clone(), t);
            }
        }
        if key != self.last_key {
            set_tooltip(&self.app, track.as_ref());
        }
        if key != self.last_key || playing != self.last_playing {
            log_track(track.as_ref());
            self.last_key = key.clone();
            self.last_playing = playing;
        }
        let _ = self.app.emit("track", &track);

        if self.cover_retries > 0 {
            self.cover_retries -= 1;
            let thumbnail = session.and_then(|s| read_cover(s).ok());
            if covers.offer_thumbnail(&key, thumbnail) {
                let _ = self.app.emit("cover", covers.current());
            }
        }
    }
}

fn set_tooltip(app: &AppHandle, track: Option<&Track>) {
    let text = match track {
        Some(t) if !t.artist.is_empty() => format!("{} — {}", t.artist, t.title),
        Some(t) => t.title.clone(),
        None => "Vinyl".to_string(),
    };
    let text: String = text.chars().take(120).collect();
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(text));
    }
}

fn fetch_artwork(app: AppHandle, key: String, track: &Track) {
    let (artist, album, title) = (track.artist.clone(), track.album.clone(), track.title.clone());
    thread::spawn(move || {
        init_winrt();
        let Some(url) = artwork::lookup(&artist, &album, &title) else { return };
        let covers = app.state::<CoverState>();
        if covers.offer_artwork(&key, url) {
            println!("  обложка из iTunes");
            let _ = app.emit("cover", covers.current());
        }
    });
}

pub fn spawn_watcher(app: AppHandle) {
    thread::spawn(move || {
        init_winrt();
        let manager = loop {
            match SessionManager::RequestAsync().and_then(|op| op.get()) {
                Ok(m) => break m,
                Err(e) => {
                    eprintln!("GSMTC недоступен: {e}");
                    thread::sleep(Duration::from_secs(5));
                }
            }
        };

        let (tx, rx) = mpsc::channel();
        let _session_changed = manager.CurrentSessionChanged(&TypedEventHandler::new(notifier(&tx)));
        let mut subscription = None;
        let mut watcher = Watcher {
            app,
            last_key: String::new(),
            last_playing: false,
            cover_retries: 0,
            timeline: None,
        };

        loop {
            follow_current(&manager, &mut subscription, &tx);
            watcher.tick(subscription.as_ref().map(|s| &s.session));

            let timeout = if watcher.cover_retries > 0 { RETRY_POLL } else { FALLBACK_POLL };
            if rx.recv_timeout(timeout).is_ok() {
                thread::sleep(EVENT_SETTLE);
                rx.try_iter().for_each(drop);
            }
        }
    });
}

fn log_track(track: Option<&Track>) {
    let Some(t) = track else {
        println!("ничего не играет");
        return;
    };
    let state = if t.playing { "▶" } else { "⏸" };
    let duration = t.duration.map_or("?".into(), fmt_time);
    println!(
        "{state} {} — {} [{}] {}/{} ({})",
        t.artist,
        t.title,
        t.album,
        fmt_time(t.position),
        duration,
        t.source
    );
}

fn fmt_time(secs: f64) -> String {
    let s = secs as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn now_ticks() -> i64 {
    let since_unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    UNIX_EPOCH_TICKS + (since_unix.as_nanos() / 100) as i64
}

fn ticks_to_secs(ticks: i64) -> f64 {
    ticks as f64 / 10_000_000.0
}

fn read_track(session: &Session) -> Result<Track> {
    let props = session.TryGetMediaPropertiesAsync()?.get()?;
    let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing;

    let timeline = session.GetTimelineProperties()?;
    let start = timeline.StartTime()?.Duration;
    let end = timeline.EndTime()?.Duration;
    let mut position = timeline.Position()?.Duration - start;
    if playing {
        let updated = timeline.LastUpdatedTime()?.UniversalTime;
        if updated > 0 {
            position += now_ticks() - updated;
        }
    }

    let duration = (end > start).then(|| ticks_to_secs(end - start));
    let mut position = ticks_to_secs(position).max(0.0);
    if let Some(d) = duration {
        position = position.min(d);
    }

    let artist = props.Artist()?.to_string();
    Ok(Track {
        title: strip_artist(&props.Title()?.to_string(), &artist),
        artist,
        album: props.AlbumTitle()?.to_string(),
        playing,
        position,
        duration,
        source: session.SourceAppUserModelId()?.to_string(),
    })
}

fn strip_artist(title: &str, artist: &str) -> String {
    if !artist.is_empty() {
        for sep in [" — ", " – ", " - "] {
            let rest = title.strip_prefix(artist).and_then(|r| r.strip_prefix(sep));
            if let Some(rest) = rest.filter(|r| !r.trim().is_empty()) {
                return rest.to_string();
            }
        }
    }
    title.to_string()
}

fn read_cover(session: &Session) -> Result<String> {
    let props = session.TryGetMediaPropertiesAsync()?.get()?;
    let stream = props.Thumbnail()?.OpenReadAsync()?.get()?;
    let size = stream.Size()? as u32;

    let buffer = Buffer::Create(size)?;
    let data = stream.ReadAsync(&buffer, size, InputStreamOptions::None)?.get()?;
    let mut bytes = vec![0u8; data.Length()? as usize];
    DataReader::FromBuffer(&data)?.ReadBytes(&mut bytes)?;

    let mime = stream.ContentType()?.to_string();
    let mime = if mime.is_empty() { "image/png".to_string() } else { mime };
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(position: f64, duration: Option<f64>, playing: bool) -> Track {
        Track {
            title: "t".into(),
            artist: "a".into(),
            album: String::new(),
            playing,
            position,
            duration,
            source: "s".into(),
        }
    }

    #[test]
    fn empty_timeline_keeps_last_known_position() {
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        let mut slot = None;

        let mut t = track(71.0, Some(246.0), true);
        Timeline::fill(&mut slot, &mut t, "k", at(0));
        assert_eq!(t.position, 71.0);

        let mut t = track(0.0, None, true);
        Timeline::fill(&mut slot, &mut t, "k", at(2));
        assert_eq!((t.position, t.duration), (73.0, Some(246.0)));

        let mut t = track(0.0, None, false);
        Timeline::fill(&mut slot, &mut t, "k", at(5));
        assert_eq!(t.position, 76.0);

        let mut t = track(0.0, None, false);
        Timeline::fill(&mut slot, &mut t, "k", at(60));
        assert_eq!(t.position, 76.0);

        let mut t = track(10.0, Some(246.0), true);
        Timeline::fill(&mut slot, &mut t, "k", at(61));
        assert_eq!(t.position, 10.0);

        let mut t = track(0.0, None, true);
        Timeline::fill(&mut slot, &mut t, "other", at(62));
        assert_eq!(t.duration, None);
    }
}
