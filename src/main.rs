#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod auth;
mod libnetease;
mod logger;
mod media;
mod memory;
mod netease;
mod paths;
mod proxy;
mod secure_store;
mod state;

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use slint::{
    Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode,
    VecModel, Weak,
};
use state::{AppState, ProxyInfo};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

slint::include_modules!();

#[derive(Default, Clone)]
struct Catalog {
    recent: Vec<netease::TrackDto>,
    daily: Vec<netease::TrackDto>,
    new_songs: Vec<netease::TrackDto>,
    playlists: Vec<netease::PlaylistDto>,
    library_recent: Vec<netease::TrackDto>,
    library_playlists: Vec<netease::PlaylistDto>,
    search_songs: Vec<netease::TrackDto>,
    search_playlists: Vec<netease::PlaylistDto>,
    detail: Vec<netease::TrackDto>,
}

#[derive(Default)]
struct Player {
    queue: Vec<netease::TrackDto>,
    index: usize,
    current: Option<netease::TrackDto>,
    resume_position: f32,
}

#[derive(Default, Clone)]
struct AudioSnapshot {
    position: f32,
    playing: bool,
    loaded: bool,
    ended: bool,
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct SavedPlayer {
    queue: Vec<netease::TrackDto>,
    index: usize,
    #[serde(default)]
    position: f32,
}

enum AudioCmd {
    Load(Vec<u8>, f32, f32),
    PlayPause,
    Volume(f32),
    Seek(f32),
    Stop,
}

#[derive(Clone)]
struct AudioHandle {
    tx: mpsc::Sender<AudioCmd>,
    snapshot: Arc<Mutex<AudioSnapshot>>,
}

fn spawn_audio() -> AudioHandle {
    let (tx, rx) = mpsc::channel::<AudioCmd>();
    let snapshot = Arc::new(Mutex::new(AudioSnapshot::default()));
    let snap = snapshot.clone();
    std::thread::spawn(move || {
        let Ok((_stream, handle)) = rodio::OutputStream::try_default() else {
            if let Ok(mut state) = snap.lock() {
                state.error = Some("无法打开系统音频输出".to_string());
            }
            return;
        };
        let Ok(sink) = rodio::Sink::try_new(&handle) else {
            if let Ok(mut state) = snap.lock() {
                state.error = Some("无法初始化音频播放器".to_string());
            }
            return;
        };
        sink.set_volume(0.7);
        let mut loaded = false;
        loop {
            match rx.recv_timeout(Duration::from_millis(120)) {
                Ok(AudioCmd::Load(bytes, duration, resume_at)) => {
                    sink.stop();
                    loaded = false;
                    match rodio::Decoder::new(Cursor::new(bytes)) {
                        Ok(decoder) => {
                            sink.append(decoder);
                            sink.play();
                            let resumed = resume_at > 0.5
                                && sink.try_seek(Duration::from_secs_f32(resume_at)).is_ok();
                            loaded = true;
                            if let Ok(mut state) = snap.lock() {
                                state.position = if resumed { resume_at } else { 0.0 };
                                state.loaded = true;
                                state.ended = false;
                                state.error = None;
                            }
                            let _ = duration;
                        }
                        Err(error) => {
                            if let Ok(mut state) = snap.lock() {
                                state.error = Some(format!("音频解码失败: {error}"));
                                state.ended = false;
                                state.playing = false;
                                state.loaded = false;
                            }
                        }
                    }
                }
                Ok(AudioCmd::PlayPause) => {
                    if sink.is_paused() {
                        sink.play();
                    } else {
                        sink.pause();
                    }
                }
                Ok(AudioCmd::Volume(value)) => sink.set_volume(value),
                Ok(AudioCmd::Seek(value)) => {
                    let _ = sink.try_seek(Duration::from_secs_f32(value.max(0.0)));
                }
                Ok(AudioCmd::Stop) => {
                    sink.stop();
                    loaded = false;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let position = sink.get_pos().as_secs_f32();
            let playing = loaded && !sink.empty() && !sink.is_paused();
            let ended = loaded && sink.empty();
            if ended {
                loaded = false;
            }
            if let Ok(mut state) = snap.lock() {
                state.position = position;
                state.playing = playing;
                state.loaded = loaded;
                if ended {
                    state.ended = true;
                }
            }
        }
    });
    AudioHandle { tx, snapshot }
}

fn color_for(id: &str) -> Color {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    let value = hasher.finish();
    Color::from_rgb_u8(
        (40 + (value & 0x7f)) as u8,
        (40 + ((value >> 8) & 0x7f)) as u8,
        (50 + ((value >> 16) & 0x7f)) as u8,
    )
}

fn format_duration(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn format_clock(seconds: f32) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn initial_for(name: &str) -> String {
    name.chars()
        .next()
        .map(|character| character.to_uppercase().to_string())
        .unwrap_or_else(|| "D".to_string())
}

fn track_item(track: &netease::TrackDto, cover: Option<Image>) -> TrackItem {
    TrackItem {
        id: track.id.clone().into(),
        title: track.name.clone().into(),
        artist: track.artist.clone().into(),
        meta: track.album.clone().into(),
        duration: format_duration(track.duration_ms).into(),
        quality: track.quality.clone().into(),
        cover: cover.unwrap_or_default(),
        tint: color_for(&track.id),
    }
}

fn playlist_items_for_ui(items: &[netease::PlaylistDto]) -> Vec<PlaylistItem> {
    items
        .iter()
        .map(|playlist| PlaylistItem {
            id: playlist.id.clone().into(),
            title: playlist.name.clone().into(),
            meta: format!("{} · {} 首", playlist.creator, playlist.track_count).into(),
            cover: Image::default(),
            tint: color_for(&playlist.id),
        })
        .collect()
}

fn artist_items_for_ui(items: &[netease::ArtistInfo]) -> Vec<PlaylistItem> {
    items
        .iter()
        .map(|artist| {
            let meta = if artist.aliases.is_empty() {
                format!("歌手 · {} 首歌曲", artist.song_count)
            } else {
                format!("歌手 · {}", artist.aliases.join(" / "))
            };
            PlaylistItem {
                id: artist.id.clone().into(),
                title: artist.name.clone().into(),
                meta: meta.into(),
                cover: Image::default(),
                tint: color_for(&artist.id),
            }
        })
        .collect()
}

fn album_items_for_ui(items: &[netease::AlbumInfo]) -> Vec<PlaylistItem> {
    items
        .iter()
        .map(|album| PlaylistItem {
            id: album.id.clone().into(),
            title: album.name.clone().into(),
            meta: if album.artist.is_empty() {
                "专辑".to_string()
            } else {
                format!("专辑 · {}", album.artist)
            }
            .into(),
            cover: Image::default(),
            tint: color_for(&album.id),
        })
        .collect()
}

#[derive(Clone, Copy)]
enum PlaylistTarget {
    Home,
    Library,
    Search,
    SearchArtists,
    SearchAlbums,
    DetailAlbums,
}

fn playlist_model_for(ui: &AppWindow, target: PlaylistTarget) -> ModelRc<PlaylistItem> {
    match target {
        PlaylistTarget::Home => ui.get_playlists(),
        PlaylistTarget::Library => ui.get_library_playlists(),
        PlaylistTarget::Search => ui.get_search_playlists(),
        PlaylistTarget::SearchArtists => ui.get_search_artists(),
        PlaylistTarget::SearchAlbums => ui.get_search_albums(),
        PlaylistTarget::DetailAlbums => ui.get_detail_albums(),
    }
}

async fn load_playlist_covers(
    state: Arc<AppState>,
    ui: Weak<AppWindow>,
    target: PlaylistTarget,
    playlists: Vec<netease::PlaylistDto>,
    limit: usize,
) {
    let semaphore = Arc::new(Semaphore::new(8));
    let mut tasks = JoinSet::new();
    for (index, playlist) in playlists.into_iter().take(limit).enumerate() {
        let size = match target {
            PlaylistTarget::Home | PlaylistTarget::Search => 256,
            PlaylistTarget::Library
            | PlaylistTarget::SearchArtists
            | PlaylistTarget::SearchAlbums
            | PlaylistTarget::DetailAlbums => 120,
        };
        let endpoint = cover_url(&playlist.cover_url, size);
        if endpoint.is_empty() {
            continue;
        }
        let state = state.clone();
        let semaphore = semaphore.clone();
        let playlist_id = playlist.id.clone();
        tasks.spawn(async move {
            let Ok(_permit) = semaphore.acquire_owned().await else {
                return None;
            };
            tokio::time::timeout(Duration::from_secs(8), fetch_cover(&state, endpoint))
                .await
                .ok()
                .flatten()
                .map(|path| (index, playlist_id, path))
        });
    }
    while let Some(joined) = tasks.join_next().await {
        let Ok(Some((index, playlist_id, path))) = joined else {
            continue;
        };
        update_ui(&ui, move |ui| {
            let model = playlist_model_for(ui, target);
            let Some(mut item) = model.row_data(index) else {
                return;
            };
            if item.id.as_str() != playlist_id.as_str() {
                return;
            }
            if let Ok(image) = Image::load_from_path(&path) {
                item.cover = image;
                model.set_row_data(index, item);
            }
        });
    }
}

async fn load_cover_refs(
    state: Arc<AppState>,
    ui: Weak<AppWindow>,
    target: PlaylistTarget,
    items: Vec<(String, String)>,
    size: u32,
    limit: usize,
) {
    let semaphore = Arc::new(Semaphore::new(8));
    let mut tasks = JoinSet::new();
    for (index, (id, url)) in items.into_iter().take(limit).enumerate() {
        let endpoint = cover_url(&url, size);
        if endpoint.is_empty() {
            continue;
        }
        let state = state.clone();
        let semaphore = semaphore.clone();
        tasks.spawn(async move {
            let Ok(_permit) = semaphore.acquire_owned().await else {
                return None;
            };
            tokio::time::timeout(Duration::from_secs(8), fetch_cover(&state, endpoint))
                .await
                .ok()
                .flatten()
                .map(|path| (index, id, path))
        });
    }
    while let Some(joined) = tasks.join_next().await {
        let Ok(Some((index, id, path))) = joined else {
            continue;
        };
        update_ui(&ui, move |ui| {
            let model = playlist_model_for(ui, target);
            let Some(mut item) = model.row_data(index) else {
                return;
            };
            if item.id.as_str() != id.as_str() {
                return;
            }
            if let Ok(image) = Image::load_from_path(&path) {
                item.cover = image;
                model.set_row_data(index, item);
            }
        });
    }
}

/// Must run on the UI thread: it decodes cached covers into Slint images.
fn track_items_for_ui(items: &[netease::TrackDto]) -> Vec<TrackItem> {
    items.iter().map(|track| track_item(track, None)).collect()
}

#[derive(Clone, Copy)]
enum TrackTarget {
    Recent,
    Daily,
    New,
    LibraryRecent,
    Search,
    Detail,
    Queue,
}

fn track_model_for(ui: &AppWindow, target: TrackTarget) -> ModelRc<TrackItem> {
    match target {
        TrackTarget::Recent => ui.get_recent_tracks(),
        TrackTarget::Daily => ui.get_daily_tracks(),
        TrackTarget::New => ui.get_new_tracks(),
        TrackTarget::LibraryRecent => ui.get_library_tracks(),
        TrackTarget::Search => ui.get_search_tracks(),
        TrackTarget::Detail => ui.get_detail_tracks(),
        TrackTarget::Queue => ui.get_queue_tracks(),
    }
}

async fn load_track_covers(
    state: Arc<AppState>,
    ui: Weak<AppWindow>,
    target: TrackTarget,
    tracks: Vec<netease::TrackDto>,
    limit: usize,
) {
    let semaphore = Arc::new(Semaphore::new(8));
    let mut tasks = JoinSet::new();
    for (index, track) in tracks.into_iter().take(limit).enumerate() {
        let size = match target {
            TrackTarget::Recent | TrackTarget::Daily | TrackTarget::New => {
                if index < 40 {
                    256
                } else {
                    120
                }
            }
            TrackTarget::LibraryRecent | TrackTarget::Search | TrackTarget::Queue => 120,
            TrackTarget::Detail => 80,
        };
        let endpoint = cover_url(&track.cover_url, size);
        if endpoint.is_empty() {
            continue;
        }
        let state = state.clone();
        let semaphore = semaphore.clone();
        let track_id = track.id.clone();
        tasks.spawn(async move {
            let Ok(_permit) = semaphore.acquire_owned().await else {
                return None;
            };
            tokio::time::timeout(Duration::from_secs(8), fetch_cover(&state, endpoint))
                .await
                .ok()
                .flatten()
                .map(|path| (index, track_id, path))
        });
    }
    while let Some(joined) = tasks.join_next().await {
        let Ok(Some((index, track_id, path))) = joined else {
            continue;
        };
        update_ui(&ui, move |ui| {
            let model = track_model_for(ui, target);
            let Some(mut item) = model.row_data(index) else {
                return;
            };
            if item.id.as_str() != track_id.as_str() {
                return;
            }
            if let Ok(image) = Image::load_from_path(&path) {
                item.cover = image;
                model.set_row_data(index, item);
            }
        });
    }
}

fn update_ui<F: FnOnce(&AppWindow) + Send + 'static>(ui: &Weak<AppWindow>, f: F) {
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            f(&ui);
        }
    });
}

fn blurred_backdrop(path: &std::path::Path) -> Option<Image> {
    let source = image::open(path).ok()?;
    // Generate the blur at a size that still looks smooth when stretched to
    // the window. A 24/180px source is what made the old backdrop look blocky.
    let small = source.resize_to_fill(640, 400, image::imageops::FilterType::Triangle);
    let blurred = small.fast_blur(24.0);
    let rgba = blurred.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    buffer.make_mut_bytes().copy_from_slice(rgba.as_raw());
    Some(Image::from_rgba8(buffer))
}

static STATUS_GENERATION: AtomicU64 = AtomicU64::new(0);
static WINDOW_RESTORED: AtomicBool = AtomicBool::new(false);
static WINDOW_MINIMIZED: AtomicBool = AtomicBool::new(false);
static WINDOW_BACKGROUNDED: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
fn watch_window_restore() {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetForegroundWindow, IsIconic};

    let _ = std::thread::Builder::new()
        .name("decro-window-watch".into())
        .spawn(|| {
            let hwnd = loop {
                if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR::null(), w!("Decro")) } {
                    break hwnd;
                }
                std::thread::sleep(Duration::from_millis(120));
            };
            let mut was_minimized = false;
            let mut was_foreground = unsafe { GetForegroundWindow() } == hwnd;
            loop {
                let minimized = unsafe { IsIconic(hwnd) }.as_bool();
                let foreground = unsafe { GetForegroundWindow() } == hwnd;
                if !was_minimized && minimized {
                    WINDOW_MINIMIZED.store(true, Ordering::Relaxed);
                }
                if was_foreground && !foreground {
                    WINDOW_BACKGROUNDED.store(true, Ordering::Relaxed);
                }
                if (was_minimized && !minimized)
                    || (!was_foreground && foreground && !minimized)
                {
                    WINDOW_RESTORED.store(true, Ordering::Relaxed);
                }
                was_minimized = minimized;
                was_foreground = foreground;
                std::thread::sleep(Duration::from_millis(40));
            }
        });
}

#[cfg(not(windows))]
fn watch_window_restore() {}

fn set_status_temp(ui: &AppWindow, message: String, duration: Duration) {
    ui.set_status_text(message.into());
    let request = STATUS_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let handle = ui.as_weak();
    slint::Timer::single_shot(duration, move || {
        if STATUS_GENERATION.load(Ordering::Relaxed) == request {
            if let Some(ui) = handle.upgrade() {
                ui.set_status_text("".into());
            }
        }
    });
}

fn switch_page(ui: &AppWindow, page: &str) {
    ui.set_page_opacity(0.0);
    ui.set_page(page.into());
    let handle = ui.as_weak();
    slint::Timer::single_shot(Duration::from_millis(16), move || {
        if let Some(ui) = handle.upgrade() {
            ui.set_page_opacity(1.0);
        }
    });
}

fn qr_image(url: &str) -> Option<Image> {
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    let modules = code.width();
    let quiet = 4usize;
    let scale = 6usize;
    let size = (modules + quiet * 2) * scale;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(size as u32, size as u32);
    let pixels = buffer.make_mut_bytes();
    pixels.fill(255);
    let colors = code.to_colors();
    for y in 0..modules {
        for x in 0..modules {
            if colors[y * modules + x] == qrcode::Color::Dark {
                let px = (x + quiet) * scale;
                let py = (y + quiet) * scale;
                for yy in py..py + scale {
                    for xx in px..px + scale {
                        let index = (yy * size + xx) * 4;
                        pixels[index] = 15;
                        pixels[index + 1] = 15;
                        pixels[index + 2] = 17;
                        pixels[index + 3] = 255;
                    }
                }
            }
        }
    }
    Some(Image::from_rgba8(buffer))
}

#[derive(Clone)]
struct Prefs {
    volume: f32,
    quality: String,
    low_power: bool,
    show_translation: bool,
    cover_accent: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            volume: 0.7,
            quality: "exhigh".to_string(),
            low_power: false,
            show_translation: true,
            cover_accent: true,
        }
    }
}

fn normalize_quality(value: &str) -> &'static str {
    match value {
        "standard" => "standard",
        "higher" => "higher",
        "exhigh" => "exhigh",
        "lossless" => "lossless",
        "hires" => "hires",
        _ => "exhigh",
    }
}

fn quality_label(value: &str) -> &'static str {
    match value {
        "standard" => "标准",
        "higher" => "较高",
        "exhigh" => "极高",
        "lossless" => "无损",
        "hires" => "Hi-Res",
        _ => "极高",
    }
}

fn load_prefs() -> Prefs {
    let value = fs::read_to_string(paths::data_dir().join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .unwrap_or(serde_json::Value::Null);
    Prefs {
        volume: value
            .get("volume")
            .and_then(|v| v.as_f64())
            .map(|v| (v as f32).clamp(0.0, 1.0))
            .unwrap_or(0.7),
        quality: normalize_quality(
            value
                .get("quality")
                .and_then(|v| v.as_str())
                .unwrap_or("exhigh"),
        )
        .to_string(),
        low_power: value
            .get("lowPower")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        show_translation: value
            .get("showTranslation")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        cover_accent: value
            .get("coverAccent")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
    }
}

fn save_prefs(prefs: &Prefs) {
    let path = paths::data_dir().join("settings.json");
    let payload = serde_json::json!({
        "volume": prefs.volume.clamp(0.0, 1.0),
        "quality": prefs.quality,
        "lowPower": prefs.low_power,
        "showTranslation": prefs.show_translation,
        "coverAccent": prefs.cover_accent,
    });
    let _ = fs::write(path, payload.to_string());
}

fn cover_url(url: &str, size: u32) -> String {
    if url.is_empty() {
        return String::new();
    }
    let base = url.split('?').next().unwrap_or(url);
    format!("{base}?param={size}y{size}")
}

fn cover_cache_base(url: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    paths::cover_dir().join(format!("{:016x}", hasher.finish()))
}

async fn fetch_cover(state: &AppState, url: String) -> Option<PathBuf> {
    if url.is_empty() {
        return None;
    }
    let base = cover_cache_base(&url);
    for extension in ["jpg", "png"] {
        let path = base.with_extension(extension);
        if path.metadata().map(|meta| meta.len() > 0).unwrap_or(false) {
            return Some(path);
        }
    }
    let response = state.api.get(&url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    if !response
        .url()
        .host_str()
        .is_some_and(state::is_allowed_audio_host)
    {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    if bytes.is_empty() || bytes.len() > 4_000_000 {
        return None;
    }
    let extension = if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else {
        return None;
    };
    let path = base.with_extension(extension);
    // Multiple lists can request the same cover at once. Write atomically so a
    // reader never sees a partially written cache file.
    let temporary = base.with_extension(format!("{extension}.{}.tmp", rand::random::<u64>()));
    if fs::write(&temporary, &bytes).is_err() {
        return None;
    }
    if fs::rename(&temporary, &path).is_err() {
        let _ = fs::remove_file(&temporary);
        return None;
    }
    Some(path)
}

static LOCAL_RECENT: OnceLock<Arc<Mutex<Vec<netease::TrackDto>>>> = OnceLock::new();
static LIKED_IDS: OnceLock<Arc<Mutex<std::collections::HashSet<String>>>> = OnceLock::new();

fn liked_ids() -> &'static Arc<Mutex<std::collections::HashSet<String>>> {
    LIKED_IDS.get_or_init(|| Arc::new(Mutex::new(std::collections::HashSet::new())))
}

fn load_local_recent() -> Vec<netease::TrackDto> {
    fs::read_to_string(paths::data_dir().join("recent-played.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn local_recent() -> &'static Arc<Mutex<Vec<netease::TrackDto>>> {
    LOCAL_RECENT.get_or_init(|| Arc::new(Mutex::new(load_local_recent())))
}

fn persist_local_recent(list: &[netease::TrackDto]) {
    if let Ok(text) = serde_json::to_string(list) {
        let _ = fs::write(paths::data_dir().join("recent-played.json"), text);
    }
}

fn remember_local_recent(track: &netease::TrackDto) -> bool {
    let Ok(mut list) = local_recent().lock() else {
        return false;
    };
    if list.first().is_some_and(|first| first.id == track.id) {
        return false;
    }
    list.retain(|item| item.id != track.id);
    list.insert(0, track.clone());
    list.truncate(200);
    persist_local_recent(&list);
    true
}

fn replace_local_recent(tracks: Vec<netease::TrackDto>) -> Vec<netease::TrackDto> {
    let mut list = tracks;
    list.truncate(200);
    if let Ok(mut current) = local_recent().lock() {
        *current = list.clone();
    }
    persist_local_recent(&list);
    list
}

fn merge_recent(
    local: &[netease::TrackDto],
    remote: &[netease::TrackDto],
) -> Vec<netease::TrackDto> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    for track in local.iter().chain(remote.iter()) {
        if seen.insert(track.id.clone()) {
            merged.push(track.clone());
        }
    }
    merged
}

fn short_description(text: &str) -> String {
    let text = text.trim().replace(['\r', '\n'], " ");
    let mut chars = text.chars();
    let short: String = chars.by_ref().take(160).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

fn load_saved_player() -> Option<SavedPlayer> {
    let text = fs::read_to_string(paths::data_dir().join("last-playback.json")).ok()?;
    let saved: SavedPlayer = serde_json::from_str(&text).ok()?;
    if saved.queue.is_empty() || saved.index >= saved.queue.len() {
        return None;
    }
    Some(saved)
}

fn save_player(player: &Arc<Mutex<Player>>, position: f32) {
    let (queue, index, position) = match player.lock() {
        Ok(player) => {
            if player.queue.is_empty() {
                return;
            }
            let index = player.index.min(player.queue.len() - 1);
            let start = index.saturating_sub(50);
            let end = (index + 150).min(player.queue.len());
            (
                player.queue[start..end].to_vec(),
                index - start,
                if position.is_finite() && position > 0.0 {
                    position
                } else {
                    0.0
                },
            )
        }
        Err(_) => return,
    };
    if let Ok(text) = serde_json::to_string(&SavedPlayer {
        queue,
        index,
        position,
    }) {
        let _ = fs::write(paths::data_dir().join("last-playback.json"), text);
    }
}

fn playlist_id_from_input(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|character| character.is_ascii_digit()) {
        return Some(trimmed.to_string());
    }
    for marker in ["id=", "playlist/"] {
        if let Some(position) = trimmed.find(marker) {
            let rest = &trimmed[position + marker.len()..];
            let id = rest
                .chars()
                .take_while(|character| character.is_ascii_digit())
                .collect::<String>();
            if !id.is_empty() {
                return Some(id);
            }
        }
    }
    None
}

fn find_track(catalog: &Catalog, id: &str) -> Option<(netease::TrackDto, Vec<netease::TrackDto>)> {
    for list in [
        &catalog.detail,
        &catalog.daily,
        &catalog.library_recent,
        &catalog.recent,
        &catalog.new_songs,
        &catalog.search_songs,
    ] {
        if let Some(track) = list.iter().find(|track| track.id == id) {
            return Some((track.clone(), list.clone()));
        }
    }
    None
}

fn start_track(
    runtime: &Arc<tokio::runtime::Runtime>,
    state: Arc<AppState>,
    ui: Weak<AppWindow>,
    audio: AudioHandle,
    _player: Arc<Mutex<Player>>,
    lyrics: Arc<Mutex<Vec<netease::LyricLine>>>,
    generation: Arc<AtomicU64>,
    prefs: Arc<Mutex<Prefs>>,
    track: netease::TrackDto,
) {
    let request = generation.fetch_add(1, Ordering::Relaxed) + 1;
    let duration = track.duration_ms as f32 / 1000.0;
    let (resume_at, queue_tracks, queue_index) = {
        let mut player = match _player.lock() {
            Ok(player) => player,
            Err(_) => return,
        };
        let same_track = player
            .current
            .as_ref()
            .is_some_and(|current| current.id == track.id);
        let position = if same_track {
            player.resume_position
        } else {
            0.0
        };
        player.resume_position = 0.0;
        let resume_at = if position > 0.5 && position + 3.0 < duration {
            position
        } else {
            0.0
        };
        (resume_at, player.queue.clone(), player.index as i32)
    };
    media::update_track(&track.name, &track.artist, &track.album, None);
    media::update_playback(true);
    if remember_local_recent(&track) {
        let local = local_recent()
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default();
        if !local.is_empty() {
            let recent_tracks = local.clone();
            let recent_ui = ui.clone();
            update_ui(&recent_ui, move |ui| {
                ui.set_recent_tracks(ModelRc::from(Rc::new(VecModel::from(track_items_for_ui(
                    &local,
                )))));
            });
            runtime.spawn(load_track_covers(
                state.clone(),
                recent_ui,
                TrackTarget::Recent,
                recent_tracks,
                80,
            ));
        }
    }
    if let Ok(mut snapshot) = audio.snapshot.lock() {
        snapshot.loaded = false;
        snapshot.playing = false;
        snapshot.ended = false;
    }
    let title = track.name.clone();
    let artist = track.artist.clone();
    let album = track.album.clone();
    let requested_quality = prefs
        .lock()
        .ok()
        .map(|prefs| prefs.quality.clone())
        .unwrap_or_else(|| "exhigh".to_string());
    let quality_text = if track.quality.is_empty() {
        quality_label(&requested_quality).to_string()
    } else {
        format!("{} · {}", quality_label(&requested_quality), track.quality)
    };
    let cover = color_for(&track.id);
    let low_power = prefs.lock().map(|prefs| prefs.low_power).unwrap_or(false);
    let track_id = track.id.clone();
    let liked = liked_ids()
        .lock()
        .map(|set| set.contains(&track.id))
        .unwrap_or(false);
    update_ui(&ui, move |ui| {
        ui.set_current_id(track_id.clone().into());
        ui.set_current_liked(liked);
        ui.set_current_title(title.into());
        ui.set_current_artist(artist.into());
        ui.set_current_album(album.into());
        ui.set_current_quality(quality_text.into());
        ui.set_position_text(format_clock(resume_at).into());
        ui.set_duration_text(format_clock(duration).into());
        ui.set_current_cover(cover);
        ui.set_current_cover_image(Image::default());
        ui.set_current_backdrop(Image::default());
        ui.set_duration(duration);
        ui.set_progress(if duration > 0.0 {
            resume_at / duration
        } else {
            0.0
        });
        ui.set_playing(true);
        ui.set_loading(true);
        ui.set_lyric_follow(true);
        ui.set_lyric_scroll_y(0.0);
        ui.set_queue_tracks(ModelRc::from(Rc::new(VecModel::from(track_items_for_ui(
            &queue_tracks,
        )))));
        ui.set_queue_index(queue_index);
        ui.set_status_text("正在加载音源…".into());
    });
    runtime.spawn(async move {
        let url =
            match netease::resolve_song_url(&state, &track.id, Some(requested_quality.as_str()))
                .await
            {
                Ok(url) => url,
                Err(error) => {
                    update_ui(&ui, move |ui| {
                        ui.set_status_text(error.into());
                        ui.set_loading(false);
                        ui.set_playing(false);
                    });
                    return;
                }
            };
        let response = match state
            .stream
            .get(&url)
            .header(reqwest::header::REFERER, "https://music.163.com/")
            .header(reqwest::header::ACCEPT, "*/*")
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let message = format!("下载音源失败: {error}");
                update_ui(&ui, move |ui| {
                    ui.set_status_text(message.into());
                    ui.set_loading(false);
                    ui.set_playing(false);
                });
                return;
            }
        };
        if !response
            .url()
            .host_str()
            .is_some_and(state::is_allowed_audio_host)
        {
            update_ui(&ui, move |ui| {
                ui.set_status_text("音源域名异常".into());
                ui.set_loading(false);
                ui.set_playing(false);
            });
            return;
        }
        if !response.status().is_success() {
            let message = format!("音源返回 HTTP {}", response.status());
            update_ui(&ui, move |ui| {
                ui.set_status_text(message.into());
                ui.set_loading(false);
                ui.set_playing(false);
            });
            return;
        }
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes.to_vec(),
            Err(error) => {
                let message = format!("读取音源失败: {error}");
                update_ui(&ui, move |ui| {
                    ui.set_status_text(message.into());
                    ui.set_loading(false);
                    ui.set_playing(false);
                });
                return;
            }
        };
        if generation.load(Ordering::Relaxed) != request {
            return;
        }
        let _ = audio.tx.send(AudioCmd::Load(bytes, duration, resume_at));
        let id = track.id.clone();
        let cover_endpoint = cover_url(&track.cover_url, 500);
        let (lyric_result, cover_path) = tokio::join!(netease::lyric(&state, id), async {
            tokio::time::timeout(Duration::from_secs(6), fetch_cover(&state, cover_endpoint))
                .await
                .ok()
                .flatten()
        },);
        if generation.load(Ordering::Relaxed) != request {
            return;
        }
        if let Ok(data) = lyric_result {
            let lines = data.lines.clone();
            if let Ok(mut current) = lyrics.lock() {
                *current = lines.clone();
            }
            update_ui(&ui, move |ui| {
                let items = lines
                    .iter()
                    .map(|line| LyricItem {
                        time: line.time as f32,
                        text: line.text.clone().into(),
                        translation: line.translation.clone().unwrap_or_default().into(),
                    })
                    .collect::<Vec<_>>();
                ui.set_lyrics(ModelRc::from(Rc::new(VecModel::from(items))));
                ui.set_lyric_index(0);
                ui.set_lyric_follow(true);
                ui.set_lyric_scroll_y(0.0);
            });
        }
        if let Some(path) = &cover_path {
            media::update_track(&track.name, &track.artist, &track.album, Some(path));
        }
        if let Some(path) = cover_path {
            update_ui(&ui, move |ui| {
                if let Ok(image) = Image::load_from_path(&path) {
                    ui.set_current_cover_image(image);
                }
                if !low_power {
                    if let Some(backdrop) = blurred_backdrop(&path) {
                        ui.set_current_backdrop(backdrop);
                    }
                }
            });
        }
        let _ = netease::report_play_start(&state, track.id.clone(), track.album_id.clone()).await;
    });
}

fn toggle_playback(
    runtime: &Arc<tokio::runtime::Runtime>,
    state: &Arc<AppState>,
    weak: &Weak<AppWindow>,
    audio: &AudioHandle,
    player: &Arc<Mutex<Player>>,
    lyrics: &Arc<Mutex<Vec<netease::LyricLine>>>,
    generation: &Arc<AtomicU64>,
    prefs: &Arc<Mutex<Prefs>>,
) {
    if let Some(ui) = weak.upgrade() {
        if ui.get_loading() {
            return;
        }
    }
    let loaded = audio
        .snapshot
        .lock()
        .map(|snapshot| snapshot.loaded)
        .unwrap_or(false);
    if !loaded {
        let track = player.lock().ok().and_then(|player| player.current.clone());
        if let Some(track) = track {
            start_track(
                runtime,
                state.clone(),
                weak.clone(),
                audio.clone(),
                player.clone(),
                lyrics.clone(),
                generation.clone(),
                prefs.clone(),
                track,
            );
        }
        return;
    }
    let _ = audio.tx.send(AudioCmd::PlayPause);
}

fn play_relative(
    delta: isize,
    runtime: &Arc<tokio::runtime::Runtime>,
    state: &Arc<AppState>,
    ui: &Weak<AppWindow>,
    audio: &AudioHandle,
    player: &Arc<Mutex<Player>>,
    lyrics: &Arc<Mutex<Vec<netease::LyricLine>>>,
    generation: &Arc<AtomicU64>,
    prefs: &Arc<Mutex<Prefs>>,
) {
    let track = {
        let mut player = match player.lock() {
            Ok(player) => player,
            Err(_) => return,
        };
        if player.queue.is_empty() {
            return;
        }
        let next = (player.index as isize + delta).rem_euclid(player.queue.len() as isize) as usize;
        player.index = next;
        let track = player.queue[next].clone();
        player.current = Some(track.clone());
        player.resume_position = 0.0;
        track
    };
    save_player(player, 0.0);
    start_track(
        runtime,
        state.clone(),
        ui.clone(),
        audio.clone(),
        player.clone(),
        lyrics.clone(),
        generation.clone(),
        prefs.clone(),
        track,
    );
}

fn load_home(
    runtime: &Arc<tokio::runtime::Runtime>,
    state: Arc<AppState>,
    ui: Weak<AppWindow>,
    catalog: Arc<Mutex<Catalog>>,
) {
    runtime.spawn(async move {
        match netease::home_feed(&state).await {
            Ok(mut home) => {
                let local = local_recent()
                    .lock()
                    .map(|list| list.clone())
                    .unwrap_or_default();
                home.recent = merge_recent(&local, &home.recent);
                home.recent = replace_local_recent(home.recent.clone());
                if let Ok(mut catalog) = catalog.lock() {
                    catalog.recent = home.recent.clone();
                    catalog.daily = home.daily.clone();
                    catalog.new_songs = home.new_songs.clone();
                    catalog.playlists = home.recommended.clone();
                }
                let recent_covers = home.recent.clone();
                let daily_covers = home.daily.clone();
                let new_covers = home.new_songs.clone();
                let recommended = home.recommended.clone();
                let recommended_count = recommended.len();
                let daily_first = home.daily.first().cloned();
                let feature_dto = home.recommended.first().cloned();
                let daily_first_cover = daily_first.clone();
                let feature_cover = feature_dto.clone();
                let daily_count = home.daily.len();
                update_ui(&ui, move |ui| {
                    ui.set_recent_tracks(ModelRc::from(Rc::new(VecModel::from(
                        track_items_for_ui(&home.recent),
                    ))));
                    ui.set_daily_tracks(ModelRc::from(Rc::new(VecModel::from(
                        track_items_for_ui(&home.daily),
                    ))));
                    ui.set_new_tracks(ModelRc::from(Rc::new(VecModel::from(track_items_for_ui(
                        &home.new_songs,
                    )))));
                    ui.set_playlists(ModelRc::from(Rc::new(VecModel::from(
                        playlist_items_for_ui(&home.recommended),
                    ))));

                    let daily_tint = daily_first
                        .as_ref()
                        .map(|track| color_for(&track.id))
                        .unwrap_or_else(|| Color::from_rgb_u8(201, 32, 39));
                    ui.set_daily_feature(PlaylistItem {
                        id: "daily".into(),
                        title: "每日推荐".into(),
                        meta: if daily_count > 0 {
                            format!("每天 6:00 更新 · {daily_count} 首").into()
                        } else {
                            "登录后同步每日推荐".into()
                        },
                        cover: Image::default(),
                        tint: daily_tint,
                    });
                    if let Some(playlist) = &feature_dto {
                        ui.set_feature_playlist(PlaylistItem {
                            id: playlist.id.clone().into(),
                            title: playlist.name.clone().into(),
                            meta: format!("{} · {} 首", playlist.creator, playlist.track_count)
                                .into(),
                            cover: Image::default(),
                            tint: color_for(&playlist.id),
                        });
                    }
                    let _ = recommended_count;
                    ui.set_status_text("".into());
                });

                if let Some(track) = daily_first_cover {
                    let endpoint = cover_url(&track.cover_url, 500);
                    if !endpoint.is_empty() {
                        let cover_state = state.clone();
                        let cover_ui = ui.clone();
                        tokio::spawn(async move {
                            if let Some(path) = tokio::time::timeout(
                                Duration::from_secs(8),
                                fetch_cover(&cover_state, endpoint),
                            )
                            .await
                            .ok()
                            .flatten()
                            {
                                update_ui(&cover_ui, move |ui| {
                                    if let Ok(image) = Image::load_from_path(&path) {
                                        let mut item = ui.get_daily_feature();
                                        item.cover = image;
                                        ui.set_daily_feature(item);
                                    }
                                });
                            }
                        });
                    }
                }
                if let Some(playlist) = feature_cover {
                    let endpoint = cover_url(&playlist.cover_url, 500);
                    if !endpoint.is_empty() {
                        let cover_state = state.clone();
                        let cover_ui = ui.clone();
                        tokio::spawn(async move {
                            if let Some(path) = tokio::time::timeout(
                                Duration::from_secs(8),
                                fetch_cover(&cover_state, endpoint),
                            )
                            .await
                            .ok()
                            .flatten()
                            {
                                update_ui(&cover_ui, move |ui| {
                                    if let Ok(image) = Image::load_from_path(&path) {
                                        let mut item = ui.get_feature_playlist();
                                        item.cover = image;
                                        ui.set_feature_playlist(item);
                                    }
                                });
                            }
                        });
                    }
                }
                for (target, tracks, limit) in [
                    (TrackTarget::Recent, recent_covers, 80usize),
                    (TrackTarget::Daily, daily_covers, 80),
                    (TrackTarget::New, new_covers, 40),
                ] {
                    tokio::spawn(load_track_covers(
                        state.clone(),
                        ui.clone(),
                        target,
                        tracks,
                        limit,
                    ));
                }
                tokio::spawn(load_playlist_covers(
                    state.clone(),
                    ui.clone(),
                    PlaylistTarget::Home,
                    recommended,
                    12,
                ));
            }
            Err(error) => update_ui(&ui, move |ui| ui.set_status_text(error.into())),
        }
    });
}

fn load_library(
    runtime: &Arc<tokio::runtime::Runtime>,
    state: Arc<AppState>,
    ui: Weak<AppWindow>,
    catalog: Arc<Mutex<Catalog>>,
) {
    runtime.spawn(async move {
        match netease::library_data(&state).await {
            Ok(mut library) => {
                let local = local_recent()
                    .lock()
                    .map(|list| list.clone())
                    .unwrap_or_default();
                library.recent = merge_recent(&local, &library.recent);
                if let Ok(mut catalog) = catalog.lock() {
                    catalog.library_playlists = library.playlists.clone();
                    catalog.library_recent = library.recent.clone();
                }
                if let Some(liked) = &library.liked {
                    if let Ok(mut set) = liked_ids().lock() {
                        *set = liked.tracks.iter().map(|track| track.id.clone()).collect();
                    }
                }
                let recent_covers = library.recent.clone();
                let playlist_count = library.playlists.len();
                let all_playlists = library.playlists.clone();
                update_ui(&ui, move |ui| {
                    ui.set_library_playlists(ModelRc::from(Rc::new(VecModel::from(
                        playlist_items_for_ui(&library.playlists),
                    ))));
                    ui.set_library_tracks(ModelRc::from(Rc::new(VecModel::from(
                        track_items_for_ui(&library.recent),
                    ))));
                    let current_id = ui.get_current_id().to_string();
                    let liked = liked_ids()
                        .lock()
                        .map(|set| set.contains(&current_id))
                        .unwrap_or(false);
                    ui.set_current_liked(liked);
                    let _ = playlist_count;
                    ui.set_status_text("".into());
                });
                tokio::spawn(load_track_covers(
                    state.clone(),
                    ui.clone(),
                    TrackTarget::LibraryRecent,
                    recent_covers,
                    80,
                ));
                tokio::spawn(load_playlist_covers(
                    state.clone(),
                    ui.clone(),
                    PlaylistTarget::Library,
                    all_playlists,
                    200,
                ));
            }
            Err(error) => update_ui(&ui, move |ui| ui.set_status_text(error.into())),
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    libnetease::init();
    logger::init(paths::log_dir());

    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let state = Arc::new(AppState::new(ProxyInfo {
        base: format!("http://{address}"),
        token: format!("{:x}", rand::random::<u64>()),
    }));
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?,
    );
    {
        let state = state.clone();
        runtime.spawn(async move {
            match tokio::net::TcpListener::from_std(listener) {
                Ok(listener) => {
                    if let Err(error) = proxy::serve(listener, (*state).clone()).await {
                        logger::log("error", "proxy", &format!("{error}"));
                    }
                }
                Err(error) => logger::log("error", "proxy", &format!("{error}")),
            }
        });
    }

    let ui = AppWindow::new()?;
    watch_window_restore();
    media::start();
    let weak = ui.as_weak();
    // A couple of full-window repaints after startup also cover the first-map
    // partial-frame variants of the same software-renderer bug.
    for delay in [500u64, 1400u64] {
        let handle = ui.as_weak();
        Timer::single_shot(Duration::from_millis(delay), move || {
            if let Some(ui) = handle.upgrade() {
                ui.set_repaint_nudge(!ui.get_repaint_nudge());
            }
        });
    }
    let catalog = Arc::new(Mutex::new(Catalog::default()));
    let player = Arc::new(Mutex::new(Player::default()));
    let lyrics = Arc::new(Mutex::new(Vec::<netease::LyricLine>::new()));
    let generation = Arc::new(AtomicU64::new(0));
    let qr_generation = Arc::new(AtomicU64::new(0));
    let search_generation = Arc::new(AtomicU64::new(0));
    let player_anim = Arc::new(AtomicU64::new(0));
    let detail_history = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let prefs = Arc::new(Mutex::new(load_prefs()));
    let snapshot = prefs
        .lock()
        .ok()
        .map(|prefs| prefs.clone())
        .unwrap_or_default();
    let audio = spawn_audio();
    ui.set_volume(snapshot.volume);
    ui.set_quality(snapshot.quality.clone().into());
    ui.set_low_power(snapshot.low_power);
    ui.set_show_translation(snapshot.show_translation);
    ui.set_cover_accent(snapshot.cover_accent);
    let _ = audio.tx.send(AudioCmd::Volume(snapshot.volume));
    ui.set_data_dir(paths::data_dir().display().to_string().into());
    ui.set_daily_feature(PlaylistItem {
        id: "daily".into(),
        title: "每日推荐".into(),
        meta: "登录后同步每日推荐".into(),
        cover: Image::default(),
        tint: Color::from_rgb_u8(201, 32, 39),
    });
    ui.set_feature_playlist(PlaylistItem {
        id: "".into(),
        title: "发现音乐".into(),
        meta: "登录后获取推荐歌单".into(),
        cover: Image::default(),
        tint: Color::from_rgb_u8(60, 40, 80),
    });
    let log_level = fs::read_to_string(paths::data_dir().join("log-level"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let log_level = match log_level.as_str() {
        "" => "info".to_string(),
        "warning" => "warn".to_string(),
        other => other.to_string(),
    };
    let _ = logger::set_level(&log_level);
    ui.set_log_level(log_level.into());
    {
        let weak = weak.clone();
        let restore_timer = Timer::default();
        restore_timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
            let ui = weak.upgrade();
            if WINDOW_MINIMIZED.swap(false, Ordering::Relaxed) {
                memory::trim_working_set("minimize", "info");
            }
            if WINDOW_BACKGROUNDED.swap(false, Ordering::Relaxed) {
                if ui.as_ref().is_some_and(|ui| !ui.get_playing()) {
                    memory::trim_working_set("background", "debug");
                }
            }
            if WINDOW_RESTORED.swap(false, Ordering::Relaxed) {
                if let Some(ui) = &ui {
                    // Slint's software renderer can keep a stale partial frame after
                    // restoring a minimized window on Windows. Changing a full-window
                    // overlay forces it to repaint every pixel.
                    ui.set_repaint_nudge(!ui.get_repaint_nudge());
                }
                memory::log_stats("restored", "info");
            }
        });
        std::mem::forget(restore_timer);
    }

    // Memory diagnostics and one idle trim, matching the Tauri build's logs.
    {
        let weak = weak.clone();
        let stats_timer = Timer::default();
        stats_timer.start(TimerMode::Repeated, Duration::from_secs(300), move || {
            memory::log_stats("periodic", "debug");
        });
        std::mem::forget(stats_timer);

        let handle = weak.clone();
        Timer::single_shot(Duration::from_millis(2500), move || {
            memory::log_stats("startup", "info");
            if let Some(ui) = handle.upgrade() {
                if !ui.get_playing() {
                    memory::trim_working_set("idle", "debug");
                }
            }
        });
    }

    ui.set_status_text("正在检查登录状态…".into());
    ui.set_login_open(true);

    let initial_local = local_recent()
        .lock()
        .map(|list| list.clone())
        .unwrap_or_default();
    if !initial_local.is_empty() {
        if let Ok(mut catalog) = catalog.lock() {
            catalog.recent = initial_local.clone();
        }
        ui.set_recent_tracks(ModelRc::from(Rc::new(VecModel::from(track_items_for_ui(
            &initial_local,
        )))));
        let cover_tracks = initial_local.clone();
        runtime.spawn(load_track_covers(
            state.clone(),
            weak.clone(),
            TrackTarget::Recent,
            cover_tracks,
            80,
        ));
    }

    if let Some(saved) = load_saved_player() {
        let track = saved.queue[saved.index].clone();
        let resume_position = if saved.position.is_finite() {
            saved.position.max(0.0)
        } else {
            0.0
        };
        if let Ok(mut player) = player.lock() {
            player.queue = saved.queue;
            player.index = saved.index;
            player.current = Some(track.clone());
            player.resume_position = resume_position;
        }
        let quality_text = if track.quality.is_empty() {
            quality_label(&snapshot.quality).to_string()
        } else {
            format!("{} · {}", quality_label(&snapshot.quality), track.quality)
        };
        ui.set_current_id(track.id.clone().into());
        ui.set_current_title(track.name.clone().into());
        ui.set_current_artist(track.artist.clone().into());
        ui.set_current_album(track.album.clone().into());
        ui.set_current_quality(quality_text.into());
        ui.set_queue_tracks(ModelRc::from(Rc::new(VecModel::from(track_items_for_ui(
            &player
                .lock()
                .map(|player| player.queue.clone())
                .unwrap_or_default(),
        )))));
        ui.set_queue_index(saved.index as i32);
        ui.set_position_text(format_clock(resume_position).into());
        ui.set_duration_text(format_duration(track.duration_ms).into());
        ui.set_current_cover(color_for(&track.id));
        let saved_duration = track.duration_ms as f32 / 1000.0;
        ui.set_duration(saved_duration);
        ui.set_progress(if saved_duration > 0.0 {
            resume_position / saved_duration
        } else {
            0.0
        });
        ui.set_playing(false);
        media::update_track(&track.name, &track.artist, &track.album, None);
        media::update_playback(false);
        let cover_endpoint = cover_url(&track.cover_url, 500);
        if !cover_endpoint.is_empty() {
            let cover_state = state.clone();
            let cover_ui = weak.clone();
            let media_title = track.name.clone();
            let media_artist = track.artist.clone();
            let media_album = track.album.clone();
            let prefs = prefs.clone();
            runtime.spawn(async move {
                if let Some(path) = fetch_cover(&cover_state, cover_endpoint).await {
                    media::update_track(&media_title, &media_artist, &media_album, Some(&path));
                    let low_power = prefs.lock().map(|prefs| prefs.low_power).unwrap_or(false);
                    update_ui(&cover_ui, move |ui| {
                        if let Ok(image) = Image::load_from_path(&path) {
                            ui.set_current_cover_image(image);
                        }
                        if !low_power {
                            if let Some(backdrop) = blurred_backdrop(&path) {
                                ui.set_current_backdrop(backdrop);
                            }
                        }
                    });
                }
            });
        }
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let qr_generation = qr_generation.clone();
        ui.on_refresh_qr(move || {
            let runtime = runtime.clone();
            let task_runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let qr_generation = qr_generation.clone();
            let request = qr_generation.fetch_add(1, Ordering::Relaxed) + 1;
            runtime.spawn(async move {
                match auth::qr_login_start(&state).await {
                    Ok(start) => {
                        let qr_url = start.qr_url.clone();
                        update_ui(&weak, move |ui| {
                            ui.set_status_text("打开网易云 App 扫码".into());
                            if let Some(image) = qr_image(&qr_url) {
                                ui.set_qr_image(image);
                            }
                        });
                        let key = start.unikey;
                        loop {
                            tokio::time::sleep(Duration::from_millis(1800)).await;
                            if qr_generation.load(Ordering::Relaxed) != request {
                                break;
                            }
                            match auth::qr_login_poll(&state, key.clone()).await {
                                Ok(poll) => {
                                    if qr_generation.load(Ordering::Relaxed) != request {
                                        break;
                                    }
                                    if let Some(account) = poll.account.clone() {
                                        update_ui(&weak, move |ui| {
                                            ui.set_logged_in(true);
                                            ui.set_login_open(false);
                                            let initial = initial_for(&account.nickname);
                                            ui.set_account_name(account.nickname.into());
                                            ui.set_account_id(account.user_id.into());
                                            ui.set_account_initial(initial.into());
                                            ui.set_status_text("登录成功，正在同步音乐库…".into());
                                        });
                                        load_home(
                                            &task_runtime,
                                            state.clone(),
                                            weak.clone(),
                                            catalog.clone(),
                                        );
                                        load_library(
                                            &task_runtime,
                                            state.clone(),
                                            weak.clone(),
                                            catalog.clone(),
                                        );
                                        break;
                                    }
                                    let message = poll.message.clone();
                                    update_ui(&weak, move |ui| ui.set_status_text(message.into()));
                                    if poll.code != 801 && poll.code != 802 && poll.code != 803 {
                                        break;
                                    }
                                }
                                Err(error) => {
                                    update_ui(&weak, move |ui| ui.set_status_text(error.into()));
                                    break;
                                }
                            }
                        }
                    }
                    Err(error) => update_ui(&weak, move |ui| ui.set_status_text(error.into())),
                }
            });
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        ui.on_import_cookie(move |cookie| {
            let runtime = runtime.clone();
            let task_runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                match auth::import_cookie(&state, cookie.to_string()).await {
                    Ok(account) => {
                        update_ui(&weak, move |ui| {
                            ui.set_logged_in(true);
                            ui.set_login_open(false);
                            let initial = initial_for(&account.nickname);
                            ui.set_account_name(account.nickname.into());
                            ui.set_account_id(account.user_id.into());
                            ui.set_account_initial(initial.into());
                            ui.set_status_text("Cookie 导入成功".into());
                        });
                        load_home(&task_runtime, state.clone(), weak.clone(), catalog.clone());
                        load_library(&task_runtime, state.clone(), weak.clone(), catalog);
                    }
                    Err(error) => update_ui(&weak, move |ui| ui.set_status_text(error.into())),
                }
            });
        });
    }

    {
        let weak = weak.clone();
        ui.on_close_login(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_login_open(false);
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        ui.on_logout(move || {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            runtime.spawn(async move {
                let _ = auth::logout(&state).await;
                update_ui(&weak, move |ui| {
                    ui.set_logged_in(false);
                    ui.set_login_open(true);
                    ui.set_account_name("".into());
                    ui.set_account_id("".into());
                    ui.set_account_initial("D".into());
                    if let Ok(mut set) = liked_ids().lock() {
                        set.clear();
                    }
                    ui.set_current_liked(false);
                    ui.set_status_text("已退出登录".into());
                    ui.invoke_refresh_qr();
                });
            });
        });
    }

    {
        let weak = weak.clone();
        let runtime = runtime.clone();
        let state = state.clone();
        let catalog = catalog.clone();
        let detail_history = detail_history.clone();
        ui.on_nav(move |page| {
            let page = page.to_string();
            if let Some(mut history) = detail_history.lock().ok() {
                history.clear();
            }
            if let Some(ui) = weak.upgrade() {
                switch_page(&ui, &page);
            }
            if page == "library" {
                load_library(&runtime, state.clone(), weak.clone(), catalog.clone());
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let search_generation = search_generation.clone();
        let detail_history = detail_history.clone();
        ui.on_search(move |keyword| {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let search_generation = search_generation.clone();
            let detail_history = detail_history.clone();
            if let Ok(mut history) = detail_history.lock() {
                history.clear();
            }
            let request = search_generation.fetch_add(1, Ordering::Relaxed) + 1;
            let keyword = keyword.to_string();
            runtime.spawn(async move {
                update_ui(&weak, |ui| {
                    switch_page(ui, "search");
                    ui.set_status_text("正在搜索…".into());
                });
                match netease::search_music(&state, keyword).await {
                    Ok(data) => {
                        if search_generation.load(Ordering::Relaxed) != request {
                            return;
                        }
                        if let Ok(mut catalog) = catalog.lock() {
                            catalog.search_songs = data.songs.clone();
                            catalog.search_playlists = data.playlists.clone();
                        }
                        let cover_items = data.playlists.clone();
                        let artist_covers = data
                            .artists
                            .iter()
                            .map(|artist| (artist.id.clone(), artist.avatar_url.clone()))
                            .collect::<Vec<_>>();
                        let album_covers = data
                            .albums
                            .iter()
                            .map(|album| (album.id.clone(), album.cover_url.clone()))
                            .collect::<Vec<_>>();
                        let song_covers = data.songs.clone();
                        update_ui(&weak, move |ui| {
                            ui.set_search_tracks(ModelRc::from(Rc::new(VecModel::from(
                                track_items_for_ui(&data.songs),
                            ))));
                            ui.set_search_playlists(ModelRc::from(Rc::new(VecModel::from(
                                playlist_items_for_ui(&data.playlists),
                            ))));
                            ui.set_search_artists(ModelRc::from(Rc::new(VecModel::from(
                                artist_items_for_ui(&data.artists),
                            ))));
                            ui.set_search_albums(ModelRc::from(Rc::new(VecModel::from(
                                album_items_for_ui(&data.albums),
                            ))));
                            let _ = data.songs.len();
                            ui.set_status_text("".into());
                        });
                        tokio::spawn(load_track_covers(
                            state.clone(),
                            weak.clone(),
                            TrackTarget::Search,
                            song_covers,
                            40,
                        ));
                        tokio::spawn(load_playlist_covers(
                            state.clone(),
                            weak.clone(),
                            PlaylistTarget::Search,
                            cover_items,
                            12,
                        ));
                        tokio::spawn(load_cover_refs(
                            state.clone(),
                            weak.clone(),
                            PlaylistTarget::SearchArtists,
                            artist_covers,
                            120,
                            18,
                        ));
                        tokio::spawn(load_cover_refs(
                            state.clone(),
                            weak.clone(),
                            PlaylistTarget::SearchAlbums,
                            album_covers,
                            120,
                            24,
                        ));
                    }
                    Err(error) => update_ui(&weak, move |ui| ui.set_status_text(error.into())),
                }
            });
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let prefs = prefs.clone();
        let detail_history = detail_history.clone();
        ui.on_open_playlist(move |id| {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let prefs = prefs.clone();
            let detail_history = detail_history.clone();
            let id = id.to_string();
            if let Some(ui) = weak.upgrade() {
                let page = ui.get_page().to_string();
                let origin = if page == "detail" {
                    "home".to_string()
                } else {
                    page
                };
                ui.set_detail_origin(origin.into());
            }
            if let Ok(mut history) = detail_history.lock() {
                history.clear();
            }
            runtime.spawn(async move {
                match netease::playlist_detail(&state, id).await {
                    Ok(playlist) => {
                        if let Ok(mut catalog) = catalog.lock() {
                            catalog.detail = playlist.tracks.clone();
                        }
                        let track_covers = playlist.tracks.clone();
                        let title = playlist.name.clone();
                        let meta = format!("{} · {} 首", playlist.creator, playlist.track_count);
                        let description = short_description(&playlist.description);
                        let cover_tint = color_for(&playlist.id);
                        let cover_endpoint = cover_url(&playlist.cover_url, 500);
                        let first_id = playlist
                            .tracks
                            .first()
                            .map(|track| track.id.clone())
                            .unwrap_or_default();
                        let detail_playlist = playlist.clone();
                        update_ui(&weak, move |ui| {
                            switch_page(ui, "detail");
                            ui.set_detail_kind("playlist".into());
                            ui.set_detail_id(detail_playlist.id.clone().into());
                            ui.set_detail_subscribed(detail_playlist.subscribed);
                            ui.set_detail_title(title.into());
                            ui.set_detail_meta(meta.into());
                            ui.set_detail_description(description.into());
                            ui.set_detail_first_id(first_id.into());
                            ui.set_detail_cover_tint(cover_tint);
                            ui.set_detail_cover_image(Image::default());
                            ui.set_detail_backdrop(Image::default());
                            ui.set_detail_albums(ModelRc::from(Rc::new(VecModel::from(Vec::<
                                PlaylistItem,
                            >::new(
                            )))));
                            ui.set_detail_tracks(ModelRc::from(Rc::new(VecModel::from(
                                track_items_for_ui(&detail_playlist.tracks),
                            ))));
                        });
                        tokio::spawn(load_track_covers(
                            state.clone(),
                            weak.clone(),
                            TrackTarget::Detail,
                            track_covers,
                            600,
                        ));
                        if !cover_endpoint.is_empty() {
                            let cover_state = state.clone();
                            let cover_ui = weak.clone();
                            let cover_title = playlist.name.clone();
                            tokio::spawn(async move {
                                if let Some(path) = tokio::time::timeout(
                                    Duration::from_secs(8),
                                    fetch_cover(&cover_state, cover_endpoint),
                                )
                                .await
                                .ok()
                                .flatten()
                                {
                                    let low_power =
                                        prefs.lock().map(|prefs| prefs.low_power).unwrap_or(false);
                                    update_ui(&cover_ui, move |ui| {
                                        if ui.get_detail_title().as_str() == cover_title {
                                            if let Ok(image) = Image::load_from_path(&path) {
                                                ui.set_detail_cover_image(image);
                                            }
                                            if !low_power {
                                                if let Some(backdrop) = blurred_backdrop(&path) {
                                                    ui.set_detail_backdrop(backdrop);
                                                }
                                            }
                                        }
                                    });
                                }
                            });
                        }
                    }
                    Err(error) => update_ui(&weak, move |ui| ui.set_status_text(error.into())),
                }
            });
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let prefs = prefs.clone();
        let detail_history = detail_history.clone();
        ui.on_open_artist(move |id| {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let prefs = prefs.clone();
            let detail_history = detail_history.clone();
            let id = id.to_string();
            if let Some(ui) = weak.upgrade() {
                if ui.get_page().as_str() == "detail" && ui.get_detail_kind().as_str() == "artist" {
                    if let Ok(mut history) = detail_history.lock() {
                        history.push(("artist".to_string(), ui.get_detail_id().to_string()));
                    }
                }
                let page = ui.get_page().to_string();
                let origin = if page == "detail" {
                    "home".to_string()
                } else {
                    page
                };
                ui.set_detail_origin(origin.into());
            }
            runtime.spawn(async move {
                match netease::artist_page(&state, id.clone()).await {
                    Ok(data) => {
                        if let Ok(mut catalog) = catalog.lock() {
                            catalog.detail = data.songs.clone();
                        }
                        let songs = data.songs.clone();
                        let albums = data.albums.clone();
                        let artist = data.artist.clone();
                        let detail_id = artist.id.clone();
                        let title = artist.name.clone();
                        let meta = format!(
                            "歌手 · {} 首歌曲 · {} 张专辑",
                            artist.song_count, artist.album_count
                        );
                        let description = if artist.brief_desc.trim().is_empty() {
                            if artist.aliases.is_empty() {
                                "网易云音乐歌手".to_string()
                            } else {
                                artist.aliases.join(" / ")
                            }
                        } else {
                            short_description(&artist.brief_desc)
                        };
                        let tint = color_for(&artist.id);
                        let first_id = songs
                            .first()
                            .map(|track| track.id.clone())
                            .unwrap_or_default();
                        let songs_for_ui = songs.clone();
                        let cover_endpoint = cover_url(&artist.avatar_url, 500);
                        let album_covers = albums
                            .iter()
                            .map(|album| (album.id.clone(), album.cover_url.clone()))
                            .collect::<Vec<_>>();
                        update_ui(&weak, move |ui| {
                            switch_page(ui, "detail");
                            ui.set_detail_kind("artist".into());
                            ui.set_detail_id(detail_id.into());
                            ui.set_detail_subscribed(false);
                            ui.set_detail_title(title.into());
                            ui.set_detail_meta(meta.into());
                            ui.set_detail_description(description.into());
                            ui.set_detail_first_id(first_id.into());
                            ui.set_detail_cover_tint(tint);
                            ui.set_detail_cover_image(Image::default());
                            ui.set_detail_backdrop(Image::default());
                            ui.set_detail_albums(ModelRc::from(Rc::new(VecModel::from(
                                album_items_for_ui(&albums),
                            ))));
                            ui.set_detail_tracks(ModelRc::from(Rc::new(VecModel::from(
                                track_items_for_ui(&songs_for_ui),
                            ))));
                        });
                        tokio::spawn(load_track_covers(
                            state.clone(),
                            weak.clone(),
                            TrackTarget::Detail,
                            songs,
                            200,
                        ));
                        tokio::spawn(load_cover_refs(
                            state.clone(),
                            weak.clone(),
                            PlaylistTarget::DetailAlbums,
                            album_covers,
                            256,
                            100,
                        ));
                        if !cover_endpoint.is_empty() {
                            let cover_state = state.clone();
                            let cover_ui = weak.clone();
                            let prefs = prefs.clone();
                            let expected_id = artist.id.clone();
                            tokio::spawn(async move {
                                if let Some(path) = tokio::time::timeout(
                                    Duration::from_secs(8),
                                    fetch_cover(&cover_state, cover_endpoint),
                                )
                                .await
                                .ok()
                                .flatten()
                                {
                                    let low_power =
                                        prefs.lock().map(|prefs| prefs.low_power).unwrap_or(false);
                                    update_ui(&cover_ui, move |ui| {
                                        if ui.get_detail_kind().as_str() != "artist"
                                            || ui.get_detail_id().as_str() != expected_id.as_str()
                                        {
                                            return;
                                        }
                                        if let Ok(image) = Image::load_from_path(&path) {
                                            ui.set_detail_cover_image(image);
                                        }
                                        if !low_power {
                                            if let Some(backdrop) = blurred_backdrop(&path) {
                                                ui.set_detail_backdrop(backdrop);
                                            }
                                        }
                                    });
                                }
                            });
                        }
                    }
                    Err(error) => update_ui(&weak, move |ui| ui.set_status_text(error.into())),
                }
            });
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let prefs = prefs.clone();
        let detail_history = detail_history.clone();
        ui.on_open_album(move |id| {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let prefs = prefs.clone();
            let detail_history = detail_history.clone();
            let id = id.to_string();
            if let Some(ui) = weak.upgrade() {
                if ui.get_page().as_str() == "detail" && ui.get_detail_kind().as_str() == "artist" {
                    if let Ok(mut history) = detail_history.lock() {
                        history.push(("artist".to_string(), ui.get_detail_id().to_string()));
                    }
                }
                let page = ui.get_page().to_string();
                let origin = if page == "detail" {
                    "home".to_string()
                } else {
                    page
                };
                ui.set_detail_origin(origin.into());
            }
            runtime.spawn(async move {
                match netease::album_page(id.clone()).await {
                    Ok(data) => {
                        if let Ok(mut catalog) = catalog.lock() {
                            catalog.detail = data.songs.clone();
                        }
                        let songs = data.songs.clone();
                        let album = data.album.clone();
                        let detail_id = album.id.clone();
                        let title = album.name.clone();
                        let meta = if album.artist.is_empty() {
                            format!("专辑 · {} 首", songs.len())
                        } else {
                            format!("专辑 · {} · {} 首", album.artist, songs.len())
                        };
                        let description = if data.description.trim().is_empty() {
                            if album.company.is_empty() {
                                "网易云音乐专辑".to_string()
                            } else {
                                album.company.clone()
                            }
                        } else {
                            short_description(&data.description)
                        };
                        let tint = color_for(&album.id);
                        let first_id = songs
                            .first()
                            .map(|track| track.id.clone())
                            .unwrap_or_default();
                        let songs_for_ui = songs.clone();
                        let cover_endpoint = cover_url(&album.cover_url, 500);
                        update_ui(&weak, move |ui| {
                            switch_page(ui, "detail");
                            ui.set_detail_kind("album".into());
                            ui.set_detail_id(detail_id.into());
                            ui.set_detail_subscribed(false);
                            ui.set_detail_title(title.into());
                            ui.set_detail_meta(meta.into());
                            ui.set_detail_description(description.into());
                            ui.set_detail_first_id(first_id.into());
                            ui.set_detail_cover_tint(tint);
                            ui.set_detail_cover_image(Image::default());
                            ui.set_detail_backdrop(Image::default());
                            ui.set_detail_albums(ModelRc::from(Rc::new(VecModel::from(Vec::<
                                PlaylistItem,
                            >::new(
                            )))));
                            ui.set_detail_tracks(ModelRc::from(Rc::new(VecModel::from(
                                track_items_for_ui(&songs_for_ui),
                            ))));
                        });
                        tokio::spawn(load_track_covers(
                            state.clone(),
                            weak.clone(),
                            TrackTarget::Detail,
                            songs,
                            200,
                        ));
                        if !cover_endpoint.is_empty() {
                            let cover_state = state.clone();
                            let cover_ui = weak.clone();
                            let prefs = prefs.clone();
                            let expected_id = album.id.clone();
                            tokio::spawn(async move {
                                if let Some(path) = tokio::time::timeout(
                                    Duration::from_secs(8),
                                    fetch_cover(&cover_state, cover_endpoint),
                                )
                                .await
                                .ok()
                                .flatten()
                                {
                                    let low_power =
                                        prefs.lock().map(|prefs| prefs.low_power).unwrap_or(false);
                                    update_ui(&cover_ui, move |ui| {
                                        if ui.get_detail_kind().as_str() != "album"
                                            || ui.get_detail_id().as_str() != expected_id.as_str()
                                        {
                                            return;
                                        }
                                        if let Ok(image) = Image::load_from_path(&path) {
                                            ui.set_detail_cover_image(image);
                                        }
                                        if !low_power {
                                            if let Some(backdrop) = blurred_backdrop(&path) {
                                                ui.set_detail_backdrop(backdrop);
                                            }
                                        }
                                    });
                                }
                            });
                        }
                    }
                    Err(error) => update_ui(&weak, move |ui| ui.set_status_text(error.into())),
                }
            });
        });
    }

    {
        let weak = weak.clone();
        let detail_history = detail_history.clone();
        ui.on_back_detail(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let kind = ui.get_detail_kind().to_string();
            if kind == "artist" || kind == "album" {
                let previous = detail_history
                    .lock()
                    .ok()
                    .and_then(|mut history| history.pop());
                if let Some((previous_kind, previous_id)) = previous {
                    ui.set_detail_kind("".into());
                    match previous_kind.as_str() {
                        "artist" => ui.invoke_open_artist(previous_id.into()),
                        "album" => ui.invoke_open_album(previous_id.into()),
                        _ => {}
                    }
                    return;
                }
                ui.set_detail_kind("".into());
                switch_page(&ui, "search");
            } else if kind == "playlist" {
                let origin = ui.get_detail_origin().to_string();
                ui.set_detail_kind("".into());
                switch_page(&ui, if origin.is_empty() { "home" } else { &origin });
            } else {
                ui.set_detail_kind("".into());
                switch_page(&ui, "home");
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let prefs = prefs.clone();
        let detail_history = detail_history.clone();
        ui.on_open_daily(move || {
            if let Ok(mut history) = detail_history.lock() {
                history.clear();
            }
            let daily = catalog
                .lock()
                .ok()
                .map(|catalog| catalog.daily.clone())
                .unwrap_or_default();
            if daily.is_empty() {
                if let Some(ui) = weak.upgrade() {
                    ui.set_status_text("请先登录同步每日推荐".into());
                    if !ui.get_logged_in() {
                        ui.set_login_open(true);
                        ui.invoke_refresh_qr();
                    }
                }
                return;
            }
            if let Ok(mut catalog) = catalog.lock() {
                catalog.detail = daily.clone();
            }
            let tracks = daily.clone();
            let first = daily.first().cloned();
            let first_id = first
                .as_ref()
                .map(|track| track.id.clone())
                .unwrap_or_default();
            let tint = first
                .as_ref()
                .map(|track| color_for(&track.id))
                .unwrap_or_else(|| Color::from_rgb_u8(201, 32, 39));
            let count = daily.len();
            let detail_tracks = daily.clone();
            update_ui(&weak, move |ui| {
                switch_page(ui, "detail");
                ui.set_detail_kind("daily".into());
                ui.set_detail_id("daily".into());
                ui.set_detail_subscribed(false);
                ui.set_detail_origin("home".into());
                ui.set_detail_title("每日推荐".into());
                ui.set_detail_meta(format!("每天 6:00 更新 · {count} 首").into());
                ui.set_detail_description("根据你的音乐口味每日更新".into());
                ui.set_detail_first_id(first_id.into());
                ui.set_detail_cover_tint(tint);
                ui.set_detail_cover_image(Image::default());
                ui.set_detail_backdrop(Image::default());
                ui.set_detail_albums(ModelRc::from(Rc::new(VecModel::from(
                    Vec::<PlaylistItem>::new(),
                ))));
                ui.set_detail_tracks(ModelRc::from(Rc::new(VecModel::from(track_items_for_ui(
                    &detail_tracks,
                )))));
            });
            runtime.spawn(load_track_covers(
                state.clone(),
                weak.clone(),
                TrackTarget::Detail,
                tracks,
                600,
            ));
            if let Some(track) = first {
                let cover_endpoint = cover_url(&track.cover_url, 500);
                if !cover_endpoint.is_empty() {
                    let cover_state = state.clone();
                    let cover_ui = weak.clone();
                    let prefs = prefs.clone();
                    runtime.spawn(async move {
                        if let Some(path) = tokio::time::timeout(
                            Duration::from_secs(8),
                            fetch_cover(&cover_state, cover_endpoint),
                        )
                        .await
                        .ok()
                        .flatten()
                        {
                            let low_power =
                                prefs.lock().map(|prefs| prefs.low_power).unwrap_or(false);
                            update_ui(&cover_ui, move |ui| {
                                if ui.get_page().as_str() != "detail"
                                    || ui.get_detail_title().as_str() != "每日推荐"
                                {
                                    return;
                                }
                                if let Ok(image) = Image::load_from_path(&path) {
                                    ui.set_detail_cover_image(image);
                                }
                                if !low_power {
                                    if let Some(backdrop) = blurred_backdrop(&path) {
                                        ui.set_detail_backdrop(backdrop);
                                    }
                                }
                            });
                        }
                    });
                }
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        let audio = audio.clone();
        let prefs = prefs.clone();
        ui.on_play_track(move |id| {
            let Some((track, context)) = catalog
                .lock()
                .ok()
                .and_then(|catalog| find_track(&catalog, id.as_str()))
                .or_else(|| {
                    let local = local_recent().lock().ok()?.clone();
                    local
                        .iter()
                        .find(|track| track.id == id.as_str())
                        .cloned()
                        .map(|track| (track, local))
                })
            else {
                return;
            };
            let index = context
                .iter()
                .position(|item| item.id == track.id)
                .unwrap_or(0);
            if let Ok(mut player) = player.lock() {
                player.queue = context;
                player.index = index;
                player.current = Some(track.clone());
                player.resume_position = 0.0;
            }
            save_player(&player, 0.0);
            start_track(
                &runtime,
                state.clone(),
                weak.clone(),
                audio.clone(),
                player.clone(),
                lyrics.clone(),
                generation.clone(),
                prefs.clone(),
                track,
            );
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        let audio = audio.clone();
        let prefs = prefs.clone();
        ui.on_play_detail(move |id| {
            let track = catalog.lock().ok().and_then(|catalog| {
                catalog
                    .detail
                    .iter()
                    .find(|track| track.id == id.as_str())
                    .cloned()
            });
            let Some(track) = track else { return };
            let context = catalog
                .lock()
                .map(|catalog| catalog.detail.clone())
                .unwrap_or_default();
            let index = context
                .iter()
                .position(|item| item.id == track.id)
                .unwrap_or(0);
            if let Ok(mut player) = player.lock() {
                player.queue = context;
                player.index = index;
                player.current = Some(track.clone());
                player.resume_position = 0.0;
            }
            save_player(&player, 0.0);
            start_track(
                &runtime,
                state.clone(),
                weak.clone(),
                audio.clone(),
                player.clone(),
                lyrics.clone(),
                generation.clone(),
                prefs.clone(),
                track,
            );
        });
    }

    {
        let weak = weak.clone();
        let prefs = prefs.clone();
        ui.on_set_low_power(move |value| {
            let mut saved = prefs.lock().map(|prefs| prefs.clone()).unwrap_or_default();
            saved.low_power = value;
            save_prefs(&saved);
            if let Some(ui) = weak.upgrade() {
                ui.set_low_power(value);
                if value {
                    ui.set_current_backdrop(Image::default());
                    ui.set_detail_backdrop(Image::default());
                }
                set_status_temp(
                    &ui,
                    if value {
                        "已开启性能模式：暂停封面模糊".to_string()
                    } else {
                        "已关闭性能模式".to_string()
                    },
                    Duration::from_millis(2400),
                );
            }
        });
    }

    {
        let weak = weak.clone();
        let prefs = prefs.clone();
        ui.on_set_cover_accent(move |value| {
            let mut saved = prefs.lock().map(|prefs| prefs.clone()).unwrap_or_default();
            saved.cover_accent = value;
            save_prefs(&saved);
            if let Some(ui) = weak.upgrade() {
                ui.set_cover_accent(value);
                set_status_temp(
                    &ui,
                    if value {
                        "已开启封面取色".to_string()
                    } else {
                        "已关闭封面取色".to_string()
                    },
                    Duration::from_millis(2400),
                );
            }
        });
    }

    {
        let weak = weak.clone();
        let prefs = prefs.clone();
        ui.on_set_show_translation(move |value| {
            let mut saved = prefs.lock().map(|prefs| prefs.clone()).unwrap_or_default();
            saved.show_translation = value;
            save_prefs(&saved);
            if let Some(ui) = weak.upgrade() {
                ui.set_show_translation(value);
                set_status_temp(
                    &ui,
                    if value {
                        "已显示歌词翻译".to_string()
                    } else {
                        "已隐藏歌词翻译".to_string()
                    },
                    Duration::from_millis(2400),
                );
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let player = player.clone();
        ui.on_toggle_queue(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let open = !ui.get_queue_open();
            ui.set_queue_open(open);
            if open {
                let tracks = player
                    .lock()
                    .map(|player| player.queue.clone())
                    .unwrap_or_default();
                runtime.spawn(load_track_covers(
                    state.clone(),
                    weak.clone(),
                    TrackTarget::Queue,
                    tracks,
                    40,
                ));
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let audio = audio.clone();
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        let prefs = prefs.clone();
        ui.on_play_queue(move |index| {
            let track = {
                let mut player = match player.lock() {
                    Ok(player) => player,
                    Err(_) => return,
                };
                let index = index.max(0) as usize;
                if index >= player.queue.len() {
                    return;
                }
                player.index = index;
                let track = player.queue[index].clone();
                player.current = Some(track.clone());
                player.resume_position = 0.0;
                track
            };
            save_player(&player, 0.0);
            if let Some(ui) = weak.upgrade() {
                ui.set_queue_open(false);
            }
            start_track(
                &runtime,
                state.clone(),
                weak.clone(),
                audio.clone(),
                player.clone(),
                lyrics.clone(),
                generation.clone(),
                prefs.clone(),
                track,
            );
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let audio = audio.clone();
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        let prefs = prefs.clone();
        ui.on_toggle_play(move || {
            toggle_playback(
                &runtime,
                &state,
                &weak,
                &audio,
                &player,
                &lyrics,
                &generation,
                &prefs,
            );
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        ui.on_toggle_like(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let id = ui.get_current_id().to_string();
            if id.is_empty() {
                return;
            }
            let liked = ui.get_current_liked();
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            runtime.spawn(async move {
                match netease::like_song(&state, id.clone(), !liked).await {
                    Ok(result) => {
                        if let Ok(mut set) = liked_ids().lock() {
                            if result.liked {
                                set.insert(id.clone());
                            } else {
                                set.remove(&id);
                            }
                        }
                        update_ui(&weak, move |ui| {
                            ui.set_current_liked(result.liked);
                            set_status_temp(
                                &ui,
                                if result.liked {
                                    "已添加到我喜欢的音乐".to_string()
                                } else {
                                    "已取消喜欢".to_string()
                                },
                                Duration::from_millis(2400),
                            );
                        });
                    }
                    Err(error) => update_ui(&weak, move |ui| ui.set_status_text(error.into())),
                }
            });
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        ui.on_toggle_subscribe(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let id = ui.get_detail_id().to_string();
            if id.is_empty() {
                return;
            }
            let subscribed = ui.get_detail_subscribed();
            let runtime = runtime.clone();
            let task_runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                match netease::subscribe_playlist(id, !subscribed).await {
                    Ok(()) => {
                        update_ui(&weak, move |ui| {
                            ui.set_detail_subscribed(!subscribed);
                            set_status_temp(
                                &ui,
                                if subscribed {
                                    "已取消收藏歌单".to_string()
                                } else {
                                    "已收藏歌单".to_string()
                                },
                                Duration::from_millis(2400),
                            );
                        });
                        load_library(&task_runtime, state, weak, catalog);
                    }
                    Err(error) => update_ui(&weak, move |ui| {
                        set_status_temp(&ui, error, Duration::from_millis(3200))
                    }),
                }
            });
        });
    }

    {
        let weak = weak.clone();
        ui.on_open_playlist_dialog(move |mode| {
            if let Some(ui) = weak.upgrade() {
                ui.set_playlist_dialog_mode(mode);
                ui.set_playlist_dialog_text("".into());
                ui.set_playlist_dialog_open(true);
            }
        });
    }

    {
        let weak = weak.clone();
        ui.on_close_playlist_dialog(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_playlist_dialog_open(false);
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        ui.on_submit_playlist_dialog(move |text| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mode = ui.get_playlist_dialog_mode().to_string();
            let input = text.trim().to_string();
            if input.is_empty() {
                set_status_temp(&ui, "请输入内容".to_string(), Duration::from_millis(2400));
                return;
            }
            let runtime = runtime.clone();
            let task_runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                if mode == "create" {
                    match netease::create_playlist(input.clone()).await {
                        Ok(_) => {
                            update_ui(&weak, move |ui| {
                                ui.set_playlist_dialog_open(false);
                                ui.set_playlist_dialog_text("".into());
                                set_status_temp(
                                    &ui,
                                    format!("已创建歌单「{input}」"),
                                    Duration::from_millis(2600),
                                );
                            });
                            load_library(&task_runtime, state, weak, catalog);
                        }
                        Err(error) => update_ui(&weak, move |ui| {
                            set_status_temp(&ui, error, Duration::from_millis(3200))
                        }),
                    }
                } else {
                    let Some(id) = playlist_id_from_input(&input) else {
                        update_ui(&weak, move |ui| {
                            set_status_temp(
                                &ui,
                                "请输入有效的歌单 ID 或链接".to_string(),
                                Duration::from_millis(3000),
                            )
                        });
                        return;
                    };
                    match netease::subscribe_playlist(id, true).await {
                        Ok(()) => {
                            update_ui(&weak, move |ui| {
                                ui.set_playlist_dialog_open(false);
                                ui.set_playlist_dialog_text("".into());
                                set_status_temp(
                                    &ui,
                                    "已添加歌单到资料库".to_string(),
                                    Duration::from_millis(2600),
                                );
                            });
                            load_library(&task_runtime, state, weak, catalog);
                        }
                        Err(error) => update_ui(&weak, move |ui| {
                            set_status_temp(&ui, error, Duration::from_millis(3200))
                        }),
                    }
                }
            });
        });
    }

    {
        let audio = audio.clone();
        let weak = weak.clone();
        let player = player.clone();
        ui.on_seek(move |value| {
            let loaded = audio
                .snapshot
                .lock()
                .map(|snapshot| snapshot.loaded)
                .unwrap_or(false);
            if !loaded {
                if let Ok(mut player) = player.lock() {
                    player.resume_position = value.max(0.0);
                }
            }
            let _ = audio.tx.send(AudioCmd::Seek(value));
            if let Some(ui) = weak.upgrade() {
                ui.set_progress(value);
                ui.set_position_text(format_clock(value).into());
            }
        });
    }

    {
        let audio = audio.clone();
        let weak = weak.clone();
        ui.on_seek_lyric(move |time| {
            let _ = audio.tx.send(AudioCmd::Seek(time));
            if let Some(ui) = weak.upgrade() {
                ui.set_progress(time);
                ui.set_lyric_follow(true);
            }
        });
    }

    {
        let audio = audio.clone();
        let prefs = prefs.clone();
        ui.on_volume_changed(move |value| {
            let _ = audio.tx.send(AudioCmd::Volume(value));
            if let Ok(mut prefs) = prefs.lock() {
                prefs.volume = value;
            }
        });
    }

    {
        let prefs = prefs.clone();
        ui.on_volume_released(move |value| {
            if let Ok(mut prefs) = prefs.lock() {
                prefs.volume = value;
                save_prefs(&prefs);
            }
        });
    }

    {
        let prefs = prefs.clone();
        let weak = weak.clone();
        ui.on_set_quality(move |value| {
            let quality = normalize_quality(value.as_str()).to_string();
            if let Ok(mut prefs) = prefs.lock() {
                prefs.quality = quality.clone();
                save_prefs(&prefs);
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_quality(quality.clone().into());
                set_status_temp(
                    &ui,
                    format!("播放音质：{}（下一首生效）", quality_label(&quality)),
                    Duration::from_millis(2400),
                );
            }
        });
    }

    {
        let weak = weak.clone();
        ui.on_set_log_level(move |value| {
            let level = value.to_string();
            match logger::set_level(&level) {
                Ok(()) => {
                    let _ = fs::write(paths::data_dir().join("log-level"), &level);
                    if let Some(ui) = weak.upgrade() {
                        ui.set_log_level(level.clone().into());
                        set_status_temp(
                            &ui,
                            format!("日志级别：{level}"),
                            Duration::from_millis(2400),
                        );
                    }
                }
                Err(error) => {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_status_text(error.into());
                    }
                }
            }
        });
    }

    {
        let weak = weak.clone();
        ui.on_open_log_folder(move || {
            if let Err(error) = logger::open_folder() {
                if let Some(ui) = weak.upgrade() {
                    ui.set_status_text(error.into());
                }
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let audio = audio.clone();
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        let prefs = prefs.clone();
        ui.on_next(move || {
            play_relative(
                1,
                &runtime,
                &state,
                &weak,
                &audio,
                &player,
                &lyrics,
                &generation,
                &prefs,
            );
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let audio = audio.clone();
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        let prefs = prefs.clone();
        ui.on_prev(move || {
            play_relative(
                -1,
                &runtime,
                &state,
                &weak,
                &audio,
                &player,
                &lyrics,
                &generation,
                &prefs,
            );
        });
    }

    {
        let weak = weak.clone();
        let player_anim = player_anim.clone();
        ui.on_open_player(move || {
            if let Some(ui) = weak.upgrade() {
                if ui.get_current_title().is_empty() {
                    set_status_temp(
                        &ui,
                        "还没有正在播放的歌曲".to_string(),
                        Duration::from_millis(2400),
                    );
                    return;
                }
                let request = player_anim.fetch_add(1, Ordering::Relaxed) + 1;
                ui.set_queue_open(false);
                ui.set_lyrics_mode(false);
                ui.set_cover_flying(true);
                ui.set_player_visible(true);
                let handle = ui.as_weak();
                let anim = player_anim.clone();
                Timer::single_shot(Duration::from_millis(16), move || {
                    if anim.load(Ordering::Relaxed) == request {
                        if let Some(ui) = handle.upgrade() {
                            ui.set_player_open(true);
                        }
                    }
                });
                let settle = ui.as_weak();
                let settle_anim = player_anim.clone();
                Timer::single_shot(Duration::from_millis(430), move || {
                    if settle_anim.load(Ordering::Relaxed) == request {
                        if let Some(ui) = settle.upgrade() {
                            ui.set_cover_flying(false);
                        }
                    }
                });
                if ui.get_lyric_follow() {
                    ui.set_lyric_scroll_y(ui.get_lyric_target_y());
                }
            }
        });
    }

    {
        let weak = weak.clone();
        let player_anim = player_anim.clone();
        ui.on_close_player(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_queue_open(false);
                let request = player_anim.fetch_add(1, Ordering::Relaxed) + 1;
                if ui.get_lyrics_mode() {
                    ui.set_player_open(false);
                    let handle = ui.as_weak();
                    let anim = player_anim.clone();
                    Timer::single_shot(Duration::from_millis(240), move || {
                        if anim.load(Ordering::Relaxed) == request {
                            if let Some(ui) = handle.upgrade() {
                                ui.set_player_visible(false);
                                ui.set_lyrics_mode(false);
                            }
                        }
                    });
                } else {
                    ui.set_cover_flying(true);
                    let handle = ui.as_weak();
                    let anim = player_anim.clone();
                    Timer::single_shot(Duration::from_millis(16), move || {
                        if anim.load(Ordering::Relaxed) == request {
                            if let Some(ui) = handle.upgrade() {
                                ui.set_player_open(false);
                            }
                        }
                    });
                    let settle = ui.as_weak();
                    let settle_anim = player_anim.clone();
                    Timer::single_shot(Duration::from_millis(440), move || {
                        if settle_anim.load(Ordering::Relaxed) == request {
                            if let Some(ui) = settle.upgrade() {
                                ui.set_player_visible(false);
                                ui.set_cover_flying(false);
                                ui.set_lyrics_mode(false);
                            }
                        }
                    });
                }
            }
        });
    }

    // Player progress + lyric highlight timer.
    {
        let weak = weak.clone();
        let audio = audio.clone();
        let lyrics = lyrics.clone();
        let runtime = runtime.clone();
        let state = state.clone();
        let player = player.clone();
        let generation = generation.clone();
        let prefs = prefs.clone();
        let mut last_media_playing = false;
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, Duration::from_millis(400), move || {
            let Some(ui) = weak.upgrade() else { return };

            match media::take_command() {
                Some(media::MediaCommand::Toggle) => toggle_playback(
                    &runtime,
                    &state,
                    &weak,
                    &audio,
                    &player,
                    &lyrics,
                    &generation,
                    &prefs,
                ),
                Some(media::MediaCommand::Next) => play_relative(
                    1,
                    &runtime,
                    &state,
                    &weak,
                    &audio,
                    &player,
                    &lyrics,
                    &generation,
                    &prefs,
                ),
                Some(media::MediaCommand::Previous) => play_relative(
                    -1,
                    &runtime,
                    &state,
                    &weak,
                    &audio,
                    &player,
                    &lyrics,
                    &generation,
                    &prefs,
                ),
                None => {}
            }

            let snapshot = audio
                .snapshot
                .lock()
                .ok()
                .map(|state| state.clone())
                .unwrap_or_default();
            if snapshot.playing != last_media_playing {
                last_media_playing = snapshot.playing;
                media::update_playback(snapshot.playing);
            }
            let duration = ui.get_duration().max(0.01);
            if snapshot.error.is_some() {
                if let Some(error) = snapshot.error.clone() {
                    ui.set_status_text(error.into());
                }
            }
            ui.set_progress(snapshot.position.min(duration));
            ui.set_position_text(format_clock(snapshot.position).into());
            if snapshot.loaded && ui.get_status_text().as_str() == "正在加载音源…" {
                ui.set_status_text("".into());
            }
            if snapshot.loaded || snapshot.error.is_some() {
                ui.set_loading(false);
            }
            ui.set_playing(snapshot.playing);
            let lines = lyrics
                .lock()
                .ok()
                .map(|lines| lines.clone())
                .unwrap_or_default();
            if !lines.is_empty() {
                let mut index = 0;
                for (i, line) in lines.iter().enumerate() {
                    if line.time as f32 <= snapshot.position {
                        index = i;
                    }
                }
                let previous = ui.get_lyric_index();
                ui.set_lyric_index(index as i32);
                if index as i32 != previous && ui.get_lyric_follow() {
                    ui.set_lyric_scroll_y(ui.get_lyric_target_y());
                }
            }
            if snapshot.ended {
                if let Ok(mut snap) = audio.snapshot.lock() {
                    snap.ended = false;
                }
                play_relative(
                    1,
                    &runtime,
                    &state,
                    &weak,
                    &audio,
                    &player,
                    &lyrics,
                    &generation,
                    &prefs,
                );
            }
        });
        std::mem::forget(timer);
    }

    // Restore session at startup.
    {
        let runtime = runtime.clone();
        let task_runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        runtime.spawn(async move {
            match auth::auth_status(&state).await {
                Ok(Some(account)) => {
                    update_ui(&weak, move |ui| {
                        ui.set_logged_in(true);
                        ui.set_login_open(false);
                        let initial = initial_for(&account.nickname);
                        ui.set_account_name(account.nickname.into());
                        ui.set_account_id(account.user_id.into());
                        ui.set_account_initial(initial.into());
                    });
                    load_home(&task_runtime, state.clone(), weak.clone(), catalog.clone());
                    load_library(&task_runtime, state.clone(), weak.clone(), catalog);
                }
                Ok(None) => {
                    update_ui(&weak, move |ui| {
                        ui.set_login_open(true);
                        ui.set_status_text("请扫码登录，或导入网页版 Cookie".into());
                        ui.invoke_refresh_qr();
                    });
                }
                Err(error) => {
                    update_ui(&weak, move |ui| {
                        ui.set_login_open(true);
                        ui.set_status_text(error.into());
                        ui.invoke_refresh_qr();
                    });
                }
            }
        });
    }

    let result = ui.run();
    let position = match audio.snapshot.lock() {
        Ok(snapshot) if snapshot.loaded => snapshot.position,
        _ => player
            .lock()
            .map(|player| player.resume_position)
            .unwrap_or(0.0),
    };
    save_player(&player, position);
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::playlist_id_from_input;

    #[test]
    fn playlist_id_accepts_plain_numbers() {
        assert_eq!(playlist_id_from_input("123456").as_deref(), Some("123456"));
    }

    #[test]
    fn playlist_id_accepts_links() {
        assert_eq!(
            playlist_id_from_input("https://music.163.com/#/playlist?id=98765&userid=1")
                .as_deref(),
            Some("98765")
        );
        assert_eq!(
            playlist_id_from_input("https://music.163.com/playlist/45678/")
                .as_deref(),
            Some("45678")
        );
    }

    #[test]
    fn playlist_id_rejects_invalid_text() {
        assert_eq!(playlist_id_from_input("not a playlist"), None);
        assert_eq!(playlist_id_from_input(""), None);
    }
}
