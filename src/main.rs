#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod auth;
mod libnetease;
mod logger;
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use slint::{Color, ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel, Weak};
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
}

enum AudioCmd {
    Load(Vec<u8>, f32),
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
                Ok(AudioCmd::Load(bytes, duration)) => {
                    sink.stop();
                    loaded = false;
                    match rodio::Decoder::new(Cursor::new(bytes)) {
                        Ok(decoder) => {
                            sink.append(decoder);
                            sink.play();
                            loaded = true;
                            if let Ok(mut state) = snap.lock() {
                                state.position = 0.0;
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

fn track_item(track: &netease::TrackDto) -> TrackItem {
    TrackItem {
        id: track.id.clone().into(),
        title: track.name.clone().into(),
        artist: track.artist.clone().into(),
        meta: track.album.clone().into(),
        duration: format_duration(track.duration_ms).into(),
        quality: track.quality.clone().into(),
        cover: color_for(&track.id),
    }
}

/// Must run on the UI thread: it decodes cached covers into Slint images.
fn playlist_items_for_ui(
    items: &[netease::PlaylistDto],
    covers: &[Option<PathBuf>],
) -> Vec<PlaylistItem> {
    items
        .iter()
        .enumerate()
        .map(|(index, playlist)| {
            let cover = covers
                .get(index)
                .and_then(|path| path.as_ref())
                .and_then(|path| Image::load_from_path(path).ok());
            PlaylistItem {
                id: playlist.id.clone().into(),
                title: playlist.name.clone().into(),
                meta: format!("{} · {} 首", playlist.creator, playlist.track_count).into(),
                cover: cover.unwrap_or_default(),
                tint: color_for(&playlist.id),
            }
        })
        .collect()
}

fn track_items(items: &[netease::TrackDto]) -> Vec<TrackItem> {
    items.iter().map(track_item).collect()
}

fn update_ui<F: FnOnce(&AppWindow) + Send + 'static>(ui: &Weak<AppWindow>, f: F) {
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            f(&ui);
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
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            volume: 0.7,
            quality: "exhigh".to_string(),
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
    }
}

fn save_prefs(prefs: &Prefs) {
    let path = paths::data_dir().join("settings.json");
    let payload = serde_json::json!({
        "volume": prefs.volume.clamp(0.0, 1.0),
        "quality": prefs.quality,
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
    fs::write(&path, &bytes).ok()?;
    Some(path)
}

async fn fetch_covers(
    state: &AppState,
    urls: Vec<String>,
    max_wait: Duration,
) -> Vec<Option<PathBuf>> {
    let mut result = vec![None; urls.len()];
    let semaphore = Arc::new(Semaphore::new(6));
    let mut tasks = JoinSet::new();
    for (index, url) in urls.into_iter().enumerate() {
        if url.is_empty() {
            continue;
        }
        let state = state.clone();
        let semaphore = semaphore.clone();
        tasks.spawn(async move {
            let Ok(_permit) = semaphore.acquire_owned().await else {
                return (index, None);
            };
            (index, fetch_cover(&state, url).await)
        });
    }
    let deadline = tokio::time::Instant::now() + max_wait;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            tasks.abort_all();
            break;
        }
        match tokio::time::timeout(deadline - now, tasks.join_next()).await {
            Ok(Some(Ok((index, path)))) => result[index] = path,
            Ok(Some(Err(_))) => {}
            Ok(None) => break,
            Err(_) => {
                tasks.abort_all();
                break;
            }
        }
    }
    result
}

fn apply_playlist_models(
    ui: &AppWindow,
    items: &[netease::PlaylistDto],
    covers: &[Option<PathBuf>],
    target: &str,
) {
    let model = ModelRc::from(Rc::new(VecModel::from(playlist_items_for_ui(items, covers))));
    match target {
        "library" => ui.set_library_playlists(model),
        "search" => ui.set_search_playlists(model),
        _ => ui.set_playlists(model),
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

fn save_player(player: &Arc<Mutex<Player>>) {
    let (queue, index) = match player.lock() {
        Ok(player) => {
            if player.queue.is_empty() {
                return;
            }
            let index = player.index.min(player.queue.len() - 1);
            let start = index.saturating_sub(50);
            let end = (index + 150).min(player.queue.len());
            (player.queue[start..end].to_vec(), index - start)
        }
        Err(_) => return,
    };
    if let Ok(text) = serde_json::to_string(&SavedPlayer { queue, index }) {
        let _ = fs::write(paths::data_dir().join("last-playback.json"), text);
    }
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
    if let Ok(mut snapshot) = audio.snapshot.lock() {
        snapshot.loaded = false;
        snapshot.playing = false;
        snapshot.ended = false;
    }
    let duration = track.duration_ms as f32 / 1000.0;
    let title = track.name.clone();
    let artist = track.artist.clone();
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
    update_ui(&ui, move |ui| {
        ui.set_current_title(title.into());
        ui.set_current_artist(artist.into());
        ui.set_current_quality(quality_text.into());
        ui.set_current_cover(cover);
        ui.set_current_cover_image(Image::default());
        ui.set_duration(duration);
        ui.set_progress(0.0);
        ui.set_playing(true);
        ui.set_loading(true);
        ui.set_status_text("正在加载音源…".into());
    });
    runtime.spawn(async move {
        let url = match netease::resolve_song_url(&state, &track.id, Some(requested_quality.as_str())).await {
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
        if !response.url().host_str().is_some_and(state::is_allowed_audio_host) {
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
        let _ = audio.tx.send(AudioCmd::Load(bytes, duration));
        let id = track.id.clone();
        let cover_endpoint = cover_url(&track.cover_url, 96);
        let (lyric_result, cover_path) = tokio::join!(
            netease::lyric(&state, id),
            fetch_cover(&state, cover_endpoint),
        );
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
                    })
                    .collect::<Vec<_>>();
                ui.set_lyrics(ModelRc::from(Rc::new(VecModel::from(items))));
                ui.set_lyric_index(0);
            });
        }
        if let Some(path) = cover_path {
            update_ui(&ui, move |ui| {
                if let Ok(image) = Image::load_from_path(&path) {
                    ui.set_current_cover_image(image);
                }
            });
        }
        let _ = netease::report_play_start(&state, track.id.clone(), track.album_id.clone()).await;
    });
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
        track
    };
    save_player(player);
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
            Ok(home) => {
                if let Ok(mut catalog) = catalog.lock() {
                    catalog.recent = home.recent.clone();
                    catalog.daily = home.daily.clone();
                    catalog.new_songs = home.new_songs.clone();
                    catalog.playlists = home.recommended.clone();
                }
                let recent = track_items(&home.recent);
                let daily = track_items(&home.daily);
                let new_songs = track_items(&home.new_songs);
                let recommended = home.recommended.clone();
                let recommended_count = recommended.len();
                update_ui(&ui, move |ui| {
                    ui.set_recent_tracks(ModelRc::from(Rc::new(VecModel::from(recent))));
                    ui.set_daily_tracks(ModelRc::from(Rc::new(VecModel::from(daily))));
                    ui.set_new_tracks(ModelRc::from(Rc::new(VecModel::from(new_songs))));
                    ui.set_playlists(ModelRc::from(Rc::new(VecModel::from(
                        playlist_items_for_ui(&home.recommended, &[]),
                    ))));
                    ui.set_status_text(format!("已加载 {recommended_count} 个推荐歌单").into());
                });
                let cover_state = state.clone();
                let cover_ui = ui.clone();
                let urls = recommended
                    .iter()
                    .map(|playlist| cover_url(&playlist.cover_url, 120))
                    .collect::<Vec<_>>();
                tokio::spawn(async move {
                    let covers =
                        fetch_covers(&cover_state, urls, Duration::from_secs(4)).await;
                    if covers.iter().any(|cover| cover.is_some()) {
                        update_ui(&cover_ui, move |ui| {
                            apply_playlist_models(ui, &recommended, &covers, "home");
                        });
                    }
                });
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
            Ok(library) => {
                if let Ok(mut catalog) = catalog.lock() {
                    catalog.library_playlists = library.playlists.clone();
                    catalog.library_recent = library.recent.clone();
                }
                let recent = track_items(&library.recent);
                let playlist_count = library.playlists.len();
                let all_playlists = library.playlists.clone();
                update_ui(&ui, move |ui| {
                    ui.set_library_playlists(ModelRc::from(Rc::new(VecModel::from(
                        playlist_items_for_ui(&library.playlists, &[]),
                    ))));
                    ui.set_library_tracks(ModelRc::from(Rc::new(VecModel::from(recent))));
                    ui.set_status_text(format!("资料库：{playlist_count} 个歌单").into());
                });
                let cover_state = state.clone();
                let cover_ui = ui.clone();
                let urls = all_playlists
                    .iter()
                    .take(30)
                    .map(|playlist| cover_url(&playlist.cover_url, 120))
                    .collect::<Vec<_>>();
                tokio::spawn(async move {
                    let covers =
                        fetch_covers(&cover_state, urls, Duration::from_secs(4)).await;
                    if covers.iter().any(|cover| cover.is_some()) {
                        update_ui(&cover_ui, move |ui| {
                            apply_playlist_models(ui, &all_playlists, &covers, "library");
                        });
                    }
                });
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
    let runtime = Arc::new(tokio::runtime::Builder::new_multi_thread().enable_all().build()?);
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
    let weak = ui.as_weak();
    let catalog = Arc::new(Mutex::new(Catalog::default()));
    let player = Arc::new(Mutex::new(Player::default()));
    let lyrics = Arc::new(Mutex::new(Vec::<netease::LyricLine>::new()));
    let generation = Arc::new(AtomicU64::new(0));
    let qr_generation = Arc::new(AtomicU64::new(0));
    let search_generation = Arc::new(AtomicU64::new(0));
    let prefs = Arc::new(Mutex::new(load_prefs()));
    let snapshot = prefs.lock().ok().map(|prefs| prefs.clone()).unwrap_or_default();
    let audio = spawn_audio();
    ui.set_volume(snapshot.volume);
    ui.set_quality(snapshot.quality.clone().into());
    let _ = audio.tx.send(AudioCmd::Volume(snapshot.volume));
    ui.set_data_dir(paths::data_dir().display().to_string().into());
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
    ui.set_status_text("正在检查登录状态…".into());
    ui.set_login_open(true);

    if let Some(saved) = load_saved_player() {
        let track = saved.queue[saved.index].clone();
        if let Ok(mut player) = player.lock() {
            player.queue = saved.queue;
            player.index = saved.index;
            player.current = Some(track.clone());
        }
        let quality_text = if track.quality.is_empty() {
            quality_label(&snapshot.quality).to_string()
        } else {
            format!("{} · {}", quality_label(&snapshot.quality), track.quality)
        };
        ui.set_current_title(track.name.clone().into());
        ui.set_current_artist(track.artist.clone().into());
        ui.set_current_quality(quality_text.into());
        ui.set_current_cover(color_for(&track.id));
        ui.set_duration(track.duration_ms as f32 / 1000.0);
        ui.set_progress(0.0);
        ui.set_playing(false);
        let cover_endpoint = cover_url(&track.cover_url, 96);
        if !cover_endpoint.is_empty() {
            let cover_state = state.clone();
            let cover_ui = weak.clone();
            runtime.spawn(async move {
                if let Some(path) = fetch_cover(&cover_state, cover_endpoint).await {
                    update_ui(&cover_ui, move |ui| {
                        if let Ok(image) = Image::load_from_path(&path) {
                            ui.set_current_cover_image(image);
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
                                            ui.set_account_name(account.nickname.into());
                                            ui.set_account_id(account.user_id.into());
                                            ui.set_status_text("登录成功，正在同步音乐库…".into());
                                        });
                                        load_home(&task_runtime, state.clone(), weak.clone(), catalog.clone());
                                        load_library(&task_runtime, state.clone(), weak.clone(), catalog.clone());
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
                            ui.set_account_name(account.nickname.into());
                            ui.set_account_id(account.user_id.into());
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
        ui.on_nav(move |page| {
            let page = page.to_string();
            if let Some(ui) = weak.upgrade() {
                ui.set_page(page.clone().into());
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
        ui.on_search(move |keyword| {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let search_generation = search_generation.clone();
            let request = search_generation.fetch_add(1, Ordering::Relaxed) + 1;
            let keyword = keyword.to_string();
            runtime.spawn(async move {
                if let Some(ui) = weak.upgrade() {
                    ui.set_page("search".into());
                    ui.set_status_text("正在搜索…".into());
                }
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
                        update_ui(&weak, move |ui| {
                            ui.set_search_tracks(ModelRc::from(Rc::new(VecModel::from(
                                track_items(&data.songs),
                            ))));
                            ui.set_search_playlists(ModelRc::from(Rc::new(VecModel::from(
                                playlist_items_for_ui(&data.playlists, &[]),
                            ))));
                            ui.set_status_text(
                                format!("搜索到 {} 首歌曲", data.songs.len()).into(),
                            );
                        });
                        let cover_state = state.clone();
                        let cover_ui = weak.clone();
                        let cover_generation = search_generation.clone();
                        let urls = cover_items
                            .iter()
                            .map(|playlist| cover_url(&playlist.cover_url, 120))
                            .collect::<Vec<_>>();
                        tokio::spawn(async move {
                            let covers =
                                fetch_covers(&cover_state, urls, Duration::from_secs(3)).await;
                            if cover_generation.load(Ordering::Relaxed) != request {
                                return;
                            }
                            if covers.iter().any(|cover| cover.is_some()) {
                                update_ui(&cover_ui, move |ui| {
                                    apply_playlist_models(
                                        ui,
                                        &cover_items,
                                        &covers,
                                        "search",
                                    );
                                });
                            }
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
        ui.on_open_playlist(move |id| {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let id = id.to_string();
            runtime.spawn(async move {
                match netease::playlist_detail(&state, id).await {
                    Ok(playlist) => {
                        if let Ok(mut catalog) = catalog.lock() {
                            catalog.detail = playlist.tracks.clone();
                        }
                        let tracks = track_items(&playlist.tracks);
                        let title = playlist.name.clone();
                        let meta = format!("{} · {} 首", playlist.creator, playlist.track_count);
                        let description = playlist.description.clone();
                        let cover_tint = color_for(&playlist.id);
                        let cover_endpoint = cover_url(&playlist.cover_url, 200);
                        update_ui(&weak, move |ui| {
                            ui.set_page("detail".into());
                            ui.set_detail_title(title.into());
                            ui.set_detail_meta(meta.into());
                            ui.set_detail_description(description.into());
                            ui.set_detail_cover_tint(cover_tint);
                            ui.set_detail_cover_image(Image::default());
                            ui.set_detail_tracks(ModelRc::from(Rc::new(VecModel::from(tracks))));
                        });
                        if !cover_endpoint.is_empty() {
                            let cover_state = state.clone();
                            let cover_ui = weak.clone();
                            let cover_title = playlist.name.clone();
                            tokio::spawn(async move {
                                if let Some(path) = fetch_cover(&cover_state, cover_endpoint).await {
                                    update_ui(&cover_ui, move |ui| {
                                        if ui.get_detail_title().as_str() == cover_title {
                                            if let Ok(image) = Image::load_from_path(&path) {
                                                ui.set_detail_cover_image(image);
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
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        let audio = audio.clone();
        let prefs = prefs.clone();
        ui.on_play_track(move |id| {
            let Some((track, context)) = catalog.lock().ok().and_then(|catalog| find_track(&catalog, id.as_str())) else {
                return;
            };
            let index = context.iter().position(|item| item.id == track.id).unwrap_or(0);
            if let Ok(mut player) = player.lock() {
                player.queue = context;
                player.index = index;
                player.current = Some(track.clone());
            }
            save_player(&player);
            start_track(&runtime, state.clone(), weak.clone(), audio.clone(), player.clone(), lyrics.clone(), generation.clone(), prefs.clone(), track);
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
            let track = catalog
                .lock()
                .ok()
                .and_then(|catalog| catalog.detail.iter().find(|track| track.id == id.as_str()).cloned());
            let Some(track) = track else { return };
            let context = catalog.lock().map(|catalog| catalog.detail.clone()).unwrap_or_default();
            let index = context.iter().position(|item| item.id == track.id).unwrap_or(0);
            if let Ok(mut player) = player.lock() {
                player.queue = context;
                player.index = index;
                player.current = Some(track.clone());
            }
            save_player(&player);
            start_track(&runtime, state.clone(), weak.clone(), audio.clone(), player.clone(), lyrics.clone(), generation.clone(), prefs.clone(), track);
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
                let track = player
                    .lock()
                    .ok()
                    .and_then(|player| player.current.clone());
                if let Some(track) = track {
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
                }
                return;
            }
            let _ = audio.tx.send(AudioCmd::PlayPause);
        });
    }

    {
        let audio = audio.clone();
        ui.on_seek(move |value| {
            let _ = audio.tx.send(AudioCmd::Seek(value));
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
                ui.set_status_text(
                    format!("播放音质：{}（下一首生效）", quality_label(&quality)).into(),
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
                        ui.set_status_text(format!("日志级别：{level}").into());
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
            play_relative(1, &runtime, &state, &weak, &audio, &player, &lyrics, &generation, &prefs);
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
            play_relative(-1, &runtime, &state, &weak, &audio, &player, &lyrics, &generation, &prefs);
        });
    }

    {
        let weak = weak.clone();
        ui.on_open_player(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_player_open(true);
            }
        });
    }

    {
        let weak = weak.clone();
        ui.on_close_player(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_player_open(false);
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
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, Duration::from_millis(400), move || {
            let Some(ui) = weak.upgrade() else { return };
            let snapshot = audio.snapshot.lock().ok().map(|state| state.clone()).unwrap_or_default();
            let duration = ui.get_duration().max(0.01);
            if snapshot.error.is_some() {
                if let Some(error) = snapshot.error.clone() {
                    ui.set_status_text(error.into());
                }
            }
            ui.set_progress(snapshot.position.min(duration));
            if snapshot.loaded || snapshot.error.is_some() {
                ui.set_loading(false);
            }
            ui.set_playing(snapshot.playing);
            let lines = lyrics.lock().ok().map(|lines| lines.clone()).unwrap_or_default();
            if !lines.is_empty() {
                let mut index = 0;
                for (i, line) in lines.iter().enumerate() {
                    if line.time as f32 <= snapshot.position {
                        index = i;
                    }
                }
                ui.set_lyric_index(index as i32);
            }
            if snapshot.ended {
                if let Ok(mut snap) = audio.snapshot.lock() {
                    snap.ended = false;
                }
                play_relative(1, &runtime, &state, &weak, &audio, &player, &lyrics, &generation, &prefs);
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
                        ui.set_account_name(account.nickname.into());
                        ui.set_account_id(account.user_id.into());
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

    ui.run()?;
    Ok(())
}
