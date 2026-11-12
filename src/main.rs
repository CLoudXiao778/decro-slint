#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod auth;
mod libnetease;
mod logger;
mod media;
mod memory;
mod netease;
mod paths;
mod secure_store;
mod state;

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
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
use state::AppState;
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
    search_lyric_tracks: Vec<netease::TrackDto>,
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
    /// Duration reported by the decoder (0 = unknown). The UI keeps using the
    /// metadata duration until this becomes available.
    duration: f32,
    playing: bool,
    loaded: bool,
    ended: bool,
    error: Option<String>,
    /// Generation of the `start_track` request this snapshot belongs to.
    /// `0` means no track is loaded / stopped.
    token: u64,
}

#[derive(Serialize, Deserialize)]
struct SavedPlayer {
    queue: Vec<netease::TrackDto>,
    index: usize,
    #[serde(default)]
    position: f32,
}

enum AudioCmd {
    Load {
        path: PathBuf,
        resume_at: f32,
        token: u64,
        autoplay: bool,
    },
    PlayPause,
    Volume(f32),
    Seek {
        value: f32,
        token: u64,
    },
    Stop {
        token: u64,
    },
}

#[derive(Clone)]
struct AudioHandle {
    tx: mpsc::Sender<AudioCmd>,
    snapshot: Arc<Mutex<AudioSnapshot>>,
}

/// A decoder always reports at least a few seconds; cap the download so a
/// broken CDN response cannot fill the disk.
const MAX_AUDIO_BYTES: u64 = 256 * 1024 * 1024;
const AUDIO_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

fn spawn_audio() -> AudioHandle {
    let (tx, rx) = mpsc::channel::<AudioCmd>();
    let snapshot = Arc::new(Mutex::new(AudioSnapshot::default()));
    let snap = snapshot.clone();
    std::thread::spawn(move || {
        let fail = |message: &str| {
            if let Ok(mut state) = snap.lock() {
                state.error = Some(message.to_string());
                // A permanent output failure is not tied to a track request;
                // use the sentinel token so the UI still surfaces it.
                state.token = u64::MAX;
                state.loaded = false;
                state.playing = false;
            }
        };
        let Ok((_stream, handle)) = rodio::OutputStream::try_default() else {
            fail("无法打开系统音频输出");
            return;
        };
        let Ok(sink) = rodio::Sink::try_new(&handle) else {
            fail("无法初始化音频播放器");
            return;
        };
        sink.set_volume(0.7);
        let mut loaded = false;
        let mut current_token: u64 = 0;
        let mut last_token: u64 = 0;
        let mut decoded_duration: Option<f32> = None;
        let mut current_path: Option<PathBuf> = None;
        let mut pending_seek: Option<(u64, f32)> = None;
        loop {
            match rx.recv_timeout(Duration::from_millis(120)) {
                Ok(AudioCmd::Load {
                    path,
                    mut resume_at,
                    token,
                    autoplay,
                }) => {
                    if token < last_token {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    last_token = token;
                    sink.stop();
                    loaded = false;
                    decoded_duration = None;
                    if let Some(old) = current_path.take() {
                        let _ = fs::remove_file(old);
                    }
                    if let Some((pending_token, value)) = pending_seek.take() {
                        if pending_token == token {
                            resume_at = value;
                        }
                    }
                    let file = match std::fs::File::open(&path) {
                        Ok(file) => file,
                        Err(error) => {
                            if let Ok(mut state) = snap.lock() {
                                state.position = 0.0;
                                state.duration = 0.0;
                                state.loaded = false;
                                state.playing = false;
                                state.ended = false;
                                state.error = Some(format!("音频文件打开失败: {error}"));
                                state.token = token;
                            }
                            let _ = fs::remove_file(&path);
                            current_token = token;
                            continue;
                        }
                    };
                    match rodio::Decoder::new(std::io::BufReader::new(file)) {
                        Ok(decoder) => {
                            // The metadata duration can describe the full song even
                            // when the CDN returns a 30 s VIP/copyright preview.
                            // Seeking past the decoded stream makes the sink end at
                            // once, and the auto-next timer then starts a different
                            // song. Clamp against the real decoded duration.
                            let real_duration = rodio::Source::total_duration(&decoder)
                                .map(|value| value.as_secs_f32());
                            // With an unknown decoded length, do not seek: the
                            // stream may be a short preview and would auto-next.
                            let resume_from = if resume_at > 0.5
                                && real_duration.is_some_and(|duration| resume_at + 3.0 < duration)
                            {
                                resume_at
                            } else {
                                0.0
                            };
                            if resume_at > 0.5 && resume_from == 0.0 {
                                logger::log(
                                    "info",
                                    "audio",
                                    &format!(
                                        "resume clamped requested={resume_at:.1}s decoded={:.1}s",
                                        real_duration.unwrap_or(0.0)
                                    ),
                                );
                            }
                            sink.append(decoder);
                            if autoplay {
                                sink.play();
                            } else {
                                sink.pause();
                            }
                            let resumed = resume_from > 0.5
                                && sink.try_seek(Duration::from_secs_f32(resume_from)).is_ok();
                            loaded = true;
                            current_token = token;
                            decoded_duration = real_duration;
                            current_path = Some(path);
                            if let Ok(mut state) = snap.lock() {
                                state.position = if resumed { resume_from } else { 0.0 };
                                state.duration = real_duration.unwrap_or(0.0);
                                state.loaded = true;
                                state.playing = autoplay;
                                state.ended = false;
                                state.error = None;
                                state.token = token;
                            }
                        }
                        Err(error) => {
                            if let Ok(mut state) = snap.lock() {
                                state.position = 0.0;
                                state.duration = 0.0;
                                state.error = Some(format!("音频解码失败: {error}"));
                                state.ended = false;
                                state.playing = false;
                                state.loaded = false;
                                state.token = token;
                            }
                            current_token = token;
                            let _ = fs::remove_file(&path);
                        }
                    }
                }
                Ok(AudioCmd::PlayPause) => {
                    if loaded {
                        if sink.is_paused() {
                            sink.play();
                        } else {
                            sink.pause();
                        }
                    }
                }
                Ok(AudioCmd::Volume(value)) => sink.set_volume(value),
                Ok(AudioCmd::Seek { value, token }) => {
                    if token == current_token && loaded {
                        let ceiling = decoded_duration
                            .map(|duration| (duration - 0.25).max(0.0))
                            .unwrap_or(f32::MAX);
                        let target = value.clamp(0.0, ceiling);
                        let _ = sink.try_seek(Duration::from_secs_f32(target));
                        pending_seek = None;
                    } else if token >= last_token {
                        // The new track is still being resolved/downloaded;
                        // apply the seek as soon as its Load arrives.
                        pending_seek = Some((token, value));
                    }
                }
                Ok(AudioCmd::Stop { token }) => {
                    if token < last_token {
                        continue;
                    }
                    last_token = token;
                    sink.stop();
                    loaded = false;
                    current_token = 0;
                    decoded_duration = None;
                    pending_seek = None;
                    if let Some(path) = current_path.take() {
                        let _ = fs::remove_file(path);
                    }
                    if let Ok(mut state) = snap.lock() {
                        state.position = 0.0;
                        state.duration = 0.0;
                        state.playing = false;
                        state.loaded = false;
                        state.ended = false;
                        state.error = None;
                        state.token = 0;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(path) = current_path.take() {
                        let _ = fs::remove_file(path);
                    }
                    break;
                }
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
                state.duration = decoded_duration.unwrap_or(0.0);
                state.token = current_token;
                if ended {
                    state.ended = true;
                }
            }
        }
    });
    AudioHandle { tx, snapshot }
}

fn ambient_background() -> Image {
    // Pre-composite the two Tauri-style ambient glows over the black shell.
    // Drawing one opaque image is much cheaper for the software renderer than
    // evaluating two full-window radial gradients on every scrolled frame.
    let width = 1200u32;
    let height = 760u32;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let pixels = buffer.make_mut_bytes();
    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - 240.0;
            let dy = y as f32 - 200.0;
            let red = (1.0 - (dx * dx + dy * dy).sqrt() / 560.0).max(0.0);
            let dx = x as f32 - 890.0;
            let dy = y as f32 - 450.0;
            let purple = (1.0 - (dx * dx + dy * dy).sqrt() / 520.0).max(0.0);
            let r = 201.0 * 0.086 * red + 107.0 * 0.070 * purple;
            let g = 32.0 * 0.086 * red + 63.0 * 0.070 * purple;
            let b = 39.0 * 0.086 * red + 160.0 * 0.070 * purple;
            let index = ((y * width + x) * 4) as usize;
            pixels[index] = r.min(255.0) as u8;
            pixels[index + 1] = g.min(255.0) as u8;
            pixels[index + 2] = b.min(255.0) as u8;
            pixels[index + 3] = 255;
        }
    }
    Image::from_rgba8(buffer)
}

/// Public web URL for the currently open detail page. IDs come from the
/// NetEase API and are numeric; anything else is rejected to keep the URL safe.
fn detail_share_url(kind: &str, id: &str) -> Option<String> {
    if kind == "daily" {
        return Some("https://music.163.com/#/discover/recommend".to_string());
    }
    if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    match kind {
        "playlist" => Some(format!("https://music.163.com/#/playlist?id={id}")),
        "artist" => Some(format!("https://music.163.com/#/artist?id={id}")),
        "album" => Some(format!("https://music.163.com/#/album?id={id}")),
        _ => None,
    }
}

fn open_external_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" || parsed.host_str() != Some("music.163.com") {
        return false;
    }
    #[cfg(windows)]
    {
        // `start` is a cmd builtin; the empty title argument protects URLs
        // containing `&` from being parsed as a second command.
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .is_ok()
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .is_ok()
    }
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
            pinned: false,
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
                pinned: false,
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
            pinned: false,
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
        let image = tokio::task::spawn_blocking(move || cover_buffer(&path))
            .await
            .ok()
            .flatten();
        let Some(image) = image else {
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
            item.cover = Image::from_rgba8(image);
            model.set_row_data(index, item);
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
        let image = tokio::task::spawn_blocking(move || cover_buffer(&path))
            .await
            .ok()
            .flatten();
        let Some(image) = image else {
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
            item.cover = Image::from_rgba8(image);
            model.set_row_data(index, item);
        });
    }
}

/// Must run on the UI thread: it decodes cached covers into Slint images.
fn track_items_for_ui(items: &[netease::TrackDto]) -> Vec<TrackItem> {
    items.iter().map(|track| track_item(track, None)).collect()
}

fn lyric_result_items_for_ui(items: &[netease::LyricSearchDto]) -> Vec<LyricResultItem> {
    items
        .iter()
        .map(|item| LyricResultItem {
            track: track_item(&item.track, None),
            lines: item.lines.join(" / ").into(),
        })
        .collect()
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
        let image = tokio::task::spawn_blocking(move || cover_buffer(&path))
            .await
            .ok()
            .flatten();
        let Some(image) = image else {
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
            item.cover = Image::from_rgba8(image);
            model.set_row_data(index, item);
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

fn blurred_backdrop_buffer(path: &std::path::Path) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let source = image::open(path).ok()?;
    // Generate the blur at a size that still looks smooth when stretched to
    // the window. A 24/180px source is what made the old backdrop look blocky.
    let small = source.resize_to_fill(640, 400, image::imageops::FilterType::Triangle);
    let blurred = small.fast_blur(24.0);
    let rgba = blurred.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    buffer.make_mut_bytes().copy_from_slice(rgba.as_raw());
    Some(buffer)
}

fn cover_buffer(path: &std::path::Path) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let rgba = image::open(path).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    buffer.make_mut_bytes().copy_from_slice(rgba.as_raw());
    Some(buffer)
}

static STATUS_GENERATION: AtomicU64 = AtomicU64::new(0);
static WINDOW_RESTORED: AtomicBool = AtomicBool::new(false);
static WINDOW_MINIMIZED: AtomicBool = AtomicBool::new(false);
static WINDOW_BACKGROUNDED: AtomicBool = AtomicBool::new(false);
/// Last value sent to SMTC. Kept in one place so a direct update from
/// `start_track`/errors and the 400 ms UI timer cannot diverge.
static MEDIA_PLAYING: AtomicBool = AtomicBool::new(false);

fn set_media_playing(playing: bool) {
    if MEDIA_PLAYING.swap(playing, Ordering::Relaxed) != playing {
        media::update_playback(playing);
    }
}

static SHUTDOWN_SAVED: AtomicBool = AtomicBool::new(false);

fn current_position(
    audio: &AudioHandle,
    player: &Arc<Mutex<Player>>,
    generation: &Arc<AtomicU64>,
) -> f32 {
    let token = generation.load(Ordering::Relaxed);
    if let Ok(snapshot) = audio.snapshot.lock() {
        if snapshot.token == token && snapshot.loaded && snapshot.position.is_finite() {
            return snapshot.position.max(0.0);
        }
    }
    player
        .lock()
        .ok()
        .map(|player| {
            if player.resume_position.is_finite() {
                player.resume_position.max(0.0)
            } else {
                0.0
            }
        })
        .unwrap_or(0.0)
}

fn save_before_exit(player: &Arc<Mutex<Player>>, audio: &AudioHandle, generation: &Arc<AtomicU64>) {
    let position = current_position(audio, player, generation);
    save_player(player, position);
    SHUTDOWN_SAVED.store(true, Ordering::Relaxed);
}

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
                if (was_minimized && !minimized) || (!was_foreground && foreground && !minimized) {
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

fn schedule_forced_exit() {
    // Safety net for the portable exe: if the event loop or the audio/player
    // snapshot ever prevents the normal shutdown path, do not leave a hidden
    // process locking the folder. Give the synchronous close-path save ample
    // time; only force-exit if it did not complete.
    let _ = std::thread::Builder::new()
        .name("decro-exit".into())
        .spawn(|| {
            for _ in 0..50 {
                if SHUTDOWN_SAVED.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            std::process::exit(0);
        });
}

fn set_status_temp(ui: &AppWindow, message: String, duration: Duration) {
    ui.set_status_text(message.clone().into());
    let request = STATUS_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let handle = ui.as_weak();
    let expected = message;
    slint::Timer::single_shot(duration, move || {
        if STATUS_GENERATION.load(Ordering::Relaxed) == request {
            if let Some(ui) = handle.upgrade() {
                if ui.get_status_text().as_str() == expected.as_str() {
                    ui.set_status_text("".into());
                }
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
    theme: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            volume: 0.7,
            quality: "exhigh".to_string(),
            low_power: false,
            show_translation: true,
            cover_accent: true,
            theme: "dark".to_string(),
        }
    }
}

fn normalize_quality(value: &str) -> &'static str {
    match value {
        "auto" => "auto",
        "standard" => "standard",
        "higher" => "higher",
        "exhigh" => "exhigh",
        "lossless" => "lossless",
        "hires" => "hires",
        _ => "exhigh",
    }
}

/// Map a track's advertised tier to the best request level for the "auto"
/// setting. The caller still falls back to exhigh/standard on failure.
fn auto_quality(quality: &str) -> &'static str {
    let quality = quality.to_ascii_lowercase();
    if quality.contains("hi-res") || quality.contains("hi res") || quality.contains("hr") {
        "hires"
    } else if quality.contains("sq") || quality.contains("无损") {
        "lossless"
    } else if quality.contains("hq") || quality.contains("极高") {
        "exhigh"
    } else if quality.contains("较高") || quality.contains("higher") {
        "higher"
    } else {
        "standard"
    }
}

fn quality_label(value: &str) -> &'static str {
    match value {
        "auto" => "自动",
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
        theme: match value.get("theme").and_then(|v| v.as_str()) {
            Some("light") => "light".to_string(),
            _ => "dark".to_string(),
        },
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct PlaylistState {
    #[serde(default)]
    pinned: Vec<String>,
    #[serde(default)]
    usage: std::collections::HashMap<String, u64>,
}

static PLAYLIST_STATE: OnceLock<Arc<Mutex<PlaylistState>>> = OnceLock::new();

fn playlist_state() -> &'static Arc<Mutex<PlaylistState>> {
    PLAYLIST_STATE.get_or_init(|| {
        let state = fs::read_to_string(paths::data_dir().join("playlist-state.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<PlaylistState>(&text).ok())
            .unwrap_or_default();
        Arc::new(Mutex::new(state))
    })
}

fn save_playlist_state(state: &PlaylistState) {
    if let Ok(text) = serde_json::to_string(state) {
        let _ = write_atomic(
            &paths::data_dir().join("playlist-state.json"),
            text.as_bytes(),
        );
    }
}

fn playlist_is_pinned(state: &PlaylistState, id: &str) -> bool {
    state.pinned.iter().any(|item| item == id)
}

static TRACK_QUALITY: OnceLock<Arc<Mutex<std::collections::HashMap<String, String>>>> =
    OnceLock::new();

fn track_quality_map() -> &'static Arc<Mutex<std::collections::HashMap<String, String>>> {
    TRACK_QUALITY.get_or_init(|| {
        let map = fs::read_to_string(paths::data_dir().join("track-quality.json"))
            .ok()
            .and_then(|text| {
                serde_json::from_str::<std::collections::HashMap<String, String>>(&text).ok()
            })
            .unwrap_or_default();
        Arc::new(Mutex::new(map))
    })
}

fn save_track_quality(map: &std::collections::HashMap<String, String>) {
    if let Ok(text) = serde_json::to_string(map) {
        let _ = write_atomic(
            &paths::data_dir().join("track-quality.json"),
            text.as_bytes(),
        );
    }
}

/// Pinned first, then most-opened, then NetEase play count and update time.
fn ordered_library_playlists(items: &[netease::PlaylistDto]) -> Vec<netease::PlaylistDto> {
    let state = playlist_state()
        .lock()
        .map(|state| state.clone())
        .unwrap_or_default();
    let mut list = items.to_vec();
    list.sort_by(|a, b| {
        let a_pinned = playlist_is_pinned(&state, &a.id);
        let b_pinned = playlist_is_pinned(&state, &b.id);
        let a_usage = state.usage.get(&a.id).copied().unwrap_or(0);
        let b_usage = state.usage.get(&b.id).copied().unwrap_or(0);
        b_pinned
            .cmp(&a_pinned)
            .then_with(|| b_usage.cmp(&a_usage))
            .then_with(|| b.play_count.cmp(&a.play_count))
            .then_with(|| b.update_time.cmp(&a.update_time))
    });
    list
}

/// The reference client keeps at least eight common playlists, plus all
/// pinned entries when the user pinned more than eight.
fn playlist_common_count(items: &[netease::PlaylistDto]) -> i32 {
    let state = playlist_state()
        .lock()
        .map(|state| state.clone())
        .unwrap_or_default();
    let pinned = items
        .iter()
        .filter(|item| playlist_is_pinned(&state, &item.id))
        .count();
    pinned.max(8) as i32
}

fn library_visible_playlists(
    ordered: &[netease::PlaylistDto],
    expanded: bool,
) -> Vec<PlaylistItem> {
    if expanded {
        return library_playlist_items(ordered);
    }
    let common = playlist_common_count(ordered).max(0) as usize;
    library_playlist_items(&ordered[..ordered.len().min(common)])
}

fn library_playlist_items(items: &[netease::PlaylistDto]) -> Vec<PlaylistItem> {
    let state = playlist_state()
        .lock()
        .map(|state| state.clone())
        .unwrap_or_default();
    items
        .iter()
        .map(|playlist| PlaylistItem {
            id: playlist.id.clone().into(),
            title: playlist.name.clone().into(),
            meta: format!("{} · {} 首", playlist.creator, playlist.track_count).into(),
            cover: Image::default(),
            tint: color_for(&playlist.id),
            pinned: playlist_is_pinned(&state, &playlist.id),
        })
        .collect()
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let _ = fs::create_dir_all(parent);
    let temporary = path.with_extension(format!("tmp.{}", rand::random::<u64>()));
    if fs::write(&temporary, bytes).is_err() {
        let _ = fs::remove_file(&temporary);
        return false;
    }
    if fs::rename(&temporary, path).is_err() {
        let _ = fs::remove_file(&temporary);
        return false;
    }
    true
}

fn save_prefs(prefs: &Prefs) {
    let path = paths::data_dir().join("settings.json");
    let payload = serde_json::json!({
        "volume": prefs.volume.clamp(0.0, 1.0),
        "quality": prefs.quality,
        "lowPower": prefs.low_power,
        "showTranslation": prefs.show_translation,
        "coverAccent": prefs.cover_accent,
        "theme": prefs.theme,
    });
    let _ = write_atomic(&path, payload.to_string().as_bytes());
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
static CURRENT_ART: OnceLock<Arc<Mutex<Option<(String, PathBuf)>>>> = OnceLock::new();
static LIKE_INFLIGHT: OnceLock<Arc<Mutex<std::collections::HashSet<String>>>> = OnceLock::new();
static SUBSCRIBE_INFLIGHT: OnceLock<Arc<Mutex<std::collections::HashSet<String>>>> =
    OnceLock::new();

fn liked_ids() -> &'static Arc<Mutex<std::collections::HashSet<String>>> {
    LIKED_IDS.get_or_init(|| Arc::new(Mutex::new(std::collections::HashSet::new())))
}

fn current_art() -> &'static Arc<Mutex<Option<(String, PathBuf)>>> {
    CURRENT_ART.get_or_init(|| Arc::new(Mutex::new(None)))
}

fn like_inflight() -> &'static Arc<Mutex<std::collections::HashSet<String>>> {
    LIKE_INFLIGHT.get_or_init(|| Arc::new(Mutex::new(std::collections::HashSet::new())))
}

fn subscribe_inflight() -> &'static Arc<Mutex<std::collections::HashSet<String>>> {
    SUBSCRIBE_INFLIGHT.get_or_init(|| Arc::new(Mutex::new(std::collections::HashSet::new())))
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
        let _ = write_atomic(
            &paths::data_dir().join("recent-played.json"),
            text.as_bytes(),
        );
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
            // Keep a generous context window (Tauri keeps up to 500). Near
            // the queue end the window is shifted so `prev` can still reach
            // earlier tracks instead of collapsing to ~50 entries.
            let start = index
                .saturating_sub(250)
                .min(player.queue.len().saturating_sub(500));
            let end = (start + 500).min(player.queue.len());
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
        let _ = write_atomic(
            &paths::data_dir().join("last-playback.json"),
            text.as_bytes(),
        );
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

/// Resolve a click to the list it actually came from, rather than guessing by
/// priority. Without this a song that also exists in a previously opened
/// detail page would hijack the queue and make `next`/`prev` useless.
fn find_track_in_context(
    catalog: &Catalog,
    context: &str,
    id: &str,
) -> Option<(netease::TrackDto, Vec<netease::TrackDto>)> {
    let list = match context {
        "new" => &catalog.new_songs,
        "daily" => &catalog.daily,
        "library-recent" => &catalog.library_recent,
        "search" => &catalog.search_songs,
        "search-lyric" => &catalog.search_lyric_tracks,
        "recent" => &catalog.recent,
        _ => return None,
    };
    list.iter()
        .find(|track| track.id == id)
        .map(|track| (track.clone(), list.clone()))
}

fn report_play_error(
    ui: &Weak<AppWindow>,
    generation: &Arc<AtomicU64>,
    request: u64,
    message: String,
) {
    if generation.load(Ordering::Relaxed) != request {
        return;
    }
    update_ui(ui, move |ui| {
        ui.set_status_text(message.into());
        ui.set_loading(false);
        ui.set_playing(false);
    });
    set_media_playing(false);
}

/// Download the (fully resolved) audio URL to the per-play cache file. Using a
/// file instead of `Response::bytes()` keeps memory bounded for Hi-Res tracks
/// and lets rodio seek without holding the whole song in RAM.
async fn download_audio(
    state: &AppState,
    url: &str,
    path: std::path::PathBuf,
) -> Result<std::path::PathBuf, String> {
    use tokio::io::AsyncWriteExt;

    let mut response = state
        .stream
        .get(url)
        .header(reqwest::header::REFERER, "https://music.163.com/")
        .header(reqwest::header::ACCEPT, "*/*")
        .send()
        .await
        .map_err(|error| format!("下载音源失败: {error}"))?;
    if !response
        .url()
        .host_str()
        .is_some_and(state::is_allowed_audio_host)
    {
        return Err("音源域名异常".to_string());
    }
    if !response.status().is_success() {
        return Err(format!("音源返回 HTTP {}", response.status()));
    }
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|error| format!("创建音频缓存失败: {error}"))?;
    let mut total: u64 = 0;
    let result: Result<(), String> = async {
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("读取音源失败: {error}"))?
        {
            total += chunk.len() as u64;
            if total > MAX_AUDIO_BYTES {
                return Err("音源文件过大，已停止下载".to_string());
            }
            file.write_all(&chunk)
                .await
                .map_err(|error| format!("写入音频缓存失败: {error}"))?;
        }
        file.flush()
            .await
            .map_err(|error| format!("写入音频缓存失败: {error}"))?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        drop(file);
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error);
    }
    Ok(path)
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
    autoplay: bool,
) {
    let request = generation.fetch_add(1, Ordering::Relaxed) + 1;
    // Invalidate any in-flight load and stop the previous sink immediately.
    // The old request's Load (if it reaches the audio thread later) is rejected
    // by the token check, so a failed new track can never leave the previous
    // song playing under the new title.
    let _ = audio.tx.send(AudioCmd::Stop { token: request });
    set_media_playing(false);
    if let Ok(mut slot) = current_art().lock() {
        *slot = None;
    }
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
            player.resume_position = 0.0;
            0.0
        };
        let resume_at = if position > 0.5 && position + 3.0 < duration {
            position
        } else {
            0.0
        };
        (resume_at, player.queue.clone(), player.index as i32)
    };
    logger::log(
        "info",
        "play",
        &format!(
            "start id={} title={} index={} resume={resume_at:.1}s",
            track.id, track.name, queue_index
        ),
    );
    media::update_track(&track.name, &track.artist, &track.album, None);
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
    let title = track.name.clone();
    let artist = track.artist.clone();
    let album = track.album.clone();
    let artist_links = if track.artists.is_empty() {
        if track.artist.is_empty() {
            Vec::new()
        } else {
            vec![ArtistLinkItem {
                id: track.artist_id.clone().into(),
                name: track.artist.clone().into(),
            }]
        }
    } else {
        track
            .artists
            .iter()
            .filter(|artist| !artist.name.is_empty())
            .map(|artist| ArtistLinkItem {
                id: artist.id.clone().into(),
                name: artist.name.clone().into(),
            })
            .collect()
    };
    let media_title = title.clone();
    let media_artist = artist.clone();
    let media_album = album.clone();
    let prefs_snapshot = prefs.lock().map(|prefs| prefs.clone()).unwrap_or_default();
    let quality_override = track_quality_map()
        .lock()
        .ok()
        .and_then(|map| map.get(&track.id).cloned());
    let selected_quality = quality_override
        .clone()
        .unwrap_or_else(|| prefs_snapshot.quality.clone());
    let requested_quality = if selected_quality == "auto" {
        auto_quality(&track.quality).to_string()
    } else {
        selected_quality.clone()
    };
    let track_quality = track.quality.clone();
    let quality_text = if selected_quality == "auto" {
        "自动".to_string()
    } else if track.quality.is_empty() {
        quality_label(&requested_quality).to_string()
    } else {
        format!("{} · {}", quality_label(&requested_quality), track.quality)
    };
    let cover = color_for(&track.id);
    let track_id = track.id.clone();
    let lyric_track_id = track.id.clone();
    let cover_track_id = track.id.clone();
    let audio_track_id = track.id.clone();
    let audio_album_id = track.album_id.clone();
    let artist_id = track.artist_id.clone();
    let album_id = track.album_id.clone();
    let quality_override_text = quality_override.unwrap_or_default();
    let cover_endpoint = cover_url(&track.cover_url, 500);
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
        ui.set_current_artist_id(artist_id.into());
        ui.set_current_album_id(album_id.into());
        ui.set_current_artists(ModelRc::from(Rc::new(VecModel::from(artist_links))));
        ui.set_current_quality(quality_text.into());
        ui.set_current_track_quality(quality_override_text.into());
        ui.set_player_menu_open(false);
        ui.set_detail_more_open(false);
        ui.set_detail_track_menu_id("".into());
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
        ui.set_playing(autoplay);
        ui.set_loading(true);
        ui.set_lyrics(ModelRc::from(Rc::new(VecModel::from(
            Vec::<LyricItem>::new(),
        ))));
        ui.set_lyric_index(0);
        ui.set_lyric_follow(true);
        ui.set_lyric_scroll_y(0.0);
        ui.set_queue_tracks(ModelRc::from(Rc::new(VecModel::from(track_items_for_ui(
            &queue_tracks,
        )))));
        ui.set_queue_index(queue_index);
        ui.set_status_text("正在加载音源…".into());
    });
    // Lyrics are fetched independently from the audio download so a slow CDN
    // does not keep the previous song's lyrics on screen.
    {
        let ui = ui.clone();
        let state = state.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        let track_id = lyric_track_id;
        runtime.spawn(async move {
            let result = netease::lyric(&state, track_id.clone()).await;
            if generation.load(Ordering::Relaxed) != request {
                return;
            }
            let lines = match result {
                Ok(data) => data.lines,
                Err(error) => {
                    logger::log("warn", "lyric", &format!("id={track_id} failed: {error}"));
                    Vec::new()
                }
            };
            if let Ok(mut current) = lyrics.lock() {
                *current = lines.clone();
            }
            update_ui(&ui, move |ui| {
                if ui.get_current_id().as_str() != track_id.as_str() {
                    return;
                }
                let items = if lines.is_empty() {
                    vec![LyricItem {
                        time: 0.0,
                        text: "暂无歌词".into(),
                        translation: "".into(),
                    }]
                } else {
                    lines
                        .iter()
                        .map(|line| LyricItem {
                            time: line.time as f32,
                            text: line.text.clone().into(),
                            translation: line.translation.clone().unwrap_or_default().into(),
                        })
                        .collect()
                };
                ui.set_lyrics(ModelRc::from(Rc::new(VecModel::from(items))));
                ui.set_lyric_index(0);
                ui.set_lyric_follow(true);
                ui.set_lyric_scroll_y(0.0);
            });
        });
    }

    // Cover and blurred backdrop are also independent of the audio download.
    {
        let ui = ui.clone();
        let state = state.clone();
        let generation = generation.clone();
        let prefs = prefs.clone();
        let track_id = cover_track_id;
        runtime.spawn(async move {
            let path =
                tokio::time::timeout(Duration::from_secs(6), fetch_cover(&state, cover_endpoint))
                    .await
                    .ok()
                    .flatten();
            if generation.load(Ordering::Relaxed) != request {
                return;
            }
            let Some(path) = path else { return };
            let image_path = path.clone();
            let image = tokio::task::spawn_blocking(move || cover_buffer(&image_path))
                .await
                .ok()
                .flatten();
            let prefs_snapshot = prefs.lock().map(|prefs| prefs.clone()).unwrap_or_default();
            let backdrop = if !prefs_snapshot.low_power && prefs_snapshot.cover_accent {
                let backdrop_path = path.clone();
                tokio::task::spawn_blocking(move || blurred_backdrop_buffer(&backdrop_path))
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };
            if generation.load(Ordering::Relaxed) != request {
                return;
            }
            if let Ok(mut slot) = current_art().lock() {
                *slot = Some((track_id.clone(), path.clone()));
            }
            media::update_track(&media_title, &media_artist, &media_album, Some(&path));
            update_ui(&ui, move |ui| {
                if ui.get_current_id().as_str() != track_id.as_str() {
                    return;
                }
                if let Some(image) = image {
                    ui.set_current_cover_image(Image::from_rgba8(image));
                }
                if let Some(backdrop) = backdrop {
                    ui.set_current_backdrop(Image::from_rgba8(backdrop));
                }
            });
        });
    }

    // Audio is streamed to a per-play cache file and only then handed to the
    // decoder; memory stays bounded and a stalled response cannot hang the
    // player forever.
    {
        let ui = ui.clone();
        let state = state.clone();
        let generation = generation.clone();
        let audio = audio.clone();
        runtime.spawn(async move {
            let resolved = match netease::resolve_song_url(
                &state,
                &audio_track_id,
                Some(requested_quality.as_str()),
            )
            .await
            {
                Ok(song) => song,
                Err(error) => {
                    report_play_error(&ui, &generation, request, error);
                    return;
                }
            };
            let quality_track_id = audio_track_id.clone();
            let actual_quality = if resolved.trial {
                format!("{} · 试听", quality_label(&resolved.level))
            } else if track_quality.is_empty() {
                quality_label(&resolved.level).to_string()
            } else {
                format!("{} · {}", quality_label(&resolved.level), track_quality)
            };
            update_ui(&ui, move |ui| {
                if ui.get_current_id().as_str() == quality_track_id.as_str() {
                    ui.set_current_quality(actual_quality.into());
                }
            });
            let path =
                paths::audio_cache_dir().join(format!("{}-{}.audio", audio_track_id, request));
            let downloaded = match tokio::time::timeout(
                AUDIO_DOWNLOAD_TIMEOUT,
                download_audio(&state, &resolved.url, path.clone()),
            )
            .await
            {
                Ok(Ok(path)) => path,
                Ok(Err(error)) => {
                    report_play_error(&ui, &generation, request, error);
                    return;
                }
                Err(_) => {
                    let _ = tokio::fs::remove_file(&path).await;
                    report_play_error(
                        &ui,
                        &generation,
                        request,
                        "音源下载超时，请检查网络".to_string(),
                    );
                    return;
                }
            };
            if generation.load(Ordering::Relaxed) != request {
                let _ = tokio::fs::remove_file(&downloaded).await;
                return;
            }
            if audio
                .tx
                .send(AudioCmd::Load {
                    path: downloaded,
                    resume_at,
                    token: request,
                    autoplay,
                })
                .is_err()
            {
                report_play_error(&ui, &generation, request, "音频播放器未启动".to_string());
                return;
            }
            let _ = netease::report_play_start(&state, audio_track_id, audio_album_id).await;
        });
    }
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
    let loaded = audio
        .snapshot
        .lock()
        .map(|snapshot| snapshot.loaded && snapshot.token == generation.load(Ordering::Relaxed))
        .unwrap_or(false);
    // Check the real audio snapshot first: the UI loading flag can stay true
    // for a while when the window is minimized, which used to make SMTC
    // play/pause silently ignore the button after next/previous.
    if loaded {
        let _ = audio.tx.send(AudioCmd::PlayPause);
        return;
    }
    if let Some(ui) = weak.upgrade() {
        if ui.get_loading() {
            set_status_temp(
                &ui,
                "歌曲还在加载，请稍候".to_string(),
                Duration::from_millis(1600),
            );
            return;
        }
    }
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
            true,
        );
    }
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
            if let Some(ui) = ui.upgrade() {
                set_status_temp(&ui, "播放队列为空".to_string(), Duration::from_millis(2000));
            }
            return;
        }
        let next = (player.index as isize + delta).rem_euclid(player.queue.len() as isize) as usize;
        player.index = next;
        let track = player.queue[next].clone();
        player.current = Some(track.clone());
        player.resume_position = 0.0;
        track
    };
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
        true,
    );
}

fn load_home(
    runtime: &Arc<tokio::runtime::Runtime>,
    state: Arc<AppState>,
    ui: Weak<AppWindow>,
    catalog: Arc<Mutex<Catalog>>,
) {
    runtime.spawn(async move {
        let epoch = state.session_epoch();
        match netease::home_feed(&state).await {
            Ok(mut home) => {
                if state.session_epoch() != epoch {
                    return;
                }
                let local = local_recent()
                    .lock()
                    .map(|list| list.clone())
                    .unwrap_or_default();
                home.recent = merge_recent(&local, &home.recent);
                let home_error = home.error.clone();
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
                        pinned: false,
                    });
                    if let Some(playlist) = &feature_dto {
                        ui.set_feature_playlist(PlaylistItem {
                            id: playlist.id.clone().into(),
                            title: playlist.name.clone().into(),
                            meta: format!("{} · {} 首", playlist.creator, playlist.track_count)
                                .into(),
                            cover: Image::default(),
                            tint: color_for(&playlist.id),
                            pinned: false,
                        });
                    }
                    let _ = recommended_count;
                    if let Some(error) = home_error {
                        ui.set_status_text(error.into());
                    } else if !ui.get_login_open() {
                        ui.set_status_text("".into());
                    }
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
                                if cover_state.session_epoch() != epoch {
                                    return;
                                }
                                let image =
                                    tokio::task::spawn_blocking(move || cover_buffer(&path))
                                        .await
                                        .ok()
                                        .flatten();
                                let Some(image) = image else { return };
                                update_ui(&cover_ui, move |ui| {
                                    let mut item = ui.get_daily_feature();
                                    item.cover = Image::from_rgba8(image);
                                    ui.set_daily_feature(item);
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
                                if cover_state.session_epoch() != epoch {
                                    return;
                                }
                                let image =
                                    tokio::task::spawn_blocking(move || cover_buffer(&path))
                                        .await
                                        .ok()
                                        .flatten();
                                let Some(image) = image else { return };
                                update_ui(&cover_ui, move |ui| {
                                    let mut item = ui.get_feature_playlist();
                                    item.cover = Image::from_rgba8(image);
                                    ui.set_feature_playlist(item);
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
            Err(error) => {
                if state.session_epoch() == epoch {
                    update_ui(&ui, move |ui| ui.set_status_text(error.into()));
                }
            }
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
        let epoch = state.session_epoch();
        match netease::library_data(&state).await {
            Ok(mut library) => {
                if state.session_epoch() != epoch {
                    return;
                }
                let local = local_recent()
                    .lock()
                    .map(|list| list.clone())
                    .unwrap_or_default();
                library.recent = merge_recent(&local, &library.recent);
                let library_error = library.error.clone();
                let ordered_playlists = ordered_library_playlists(&library.playlists);
                let common_count = playlist_common_count(&ordered_playlists);
                if let Ok(mut catalog) = catalog.lock() {
                    catalog.library_playlists = ordered_playlists.clone();
                    catalog.library_recent = library.recent.clone();
                }
                let likes_pending = like_inflight()
                    .lock()
                    .map(|set| !set.is_empty())
                    .unwrap_or(false);
                if !likes_pending {
                    if let Some(liked) = &library.liked {
                        if let Ok(mut set) = liked_ids().lock() {
                            *set = liked.tracks.iter().map(|track| track.id.clone()).collect();
                        }
                    }
                }
                let recent_covers = library.recent.clone();
                let all_playlists = ordered_playlists.clone();
                update_ui(&ui, move |ui| {
                    let expanded = ui.get_library_playlists_expanded();
                    ui.set_library_playlists(ModelRc::from(Rc::new(VecModel::from(
                        library_visible_playlists(&ordered_playlists, expanded),
                    ))));
                    ui.set_library_common_count(common_count);
                    ui.set_library_playlists_total(ordered_playlists.len() as i32);
                    ui.set_library_tracks(ModelRc::from(Rc::new(VecModel::from(
                        track_items_for_ui(&library.recent),
                    ))));
                    let current_id = ui.get_current_id().to_string();
                    let liked = liked_ids()
                        .lock()
                        .map(|set| set.contains(&current_id))
                        .unwrap_or(false);
                    ui.set_current_liked(liked);
                    if let Some(error) = library_error {
                        ui.set_status_text(error.into());
                    } else if !ui.get_login_open() {
                        ui.set_status_text("".into());
                    }
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
            Err(error) => {
                if state.session_epoch() == epoch {
                    update_ui(&ui, move |ui| ui.set_status_text(error.into()));
                }
            }
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    libnetease::init();
    logger::init(paths::log_dir());
    paths::clean_audio_cache();
    paths::trim_cover_cache(3000);

    let state = Arc::new(AppState::new());
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?,
    );

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
    let detail_generation = Arc::new(AtomicU64::new(0));
    let player_anim = Arc::new(AtomicU64::new(0));
    let detail_history = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let prefs = Arc::new(Mutex::new(load_prefs()));
    let snapshot = prefs
        .lock()
        .ok()
        .map(|prefs| prefs.clone())
        .unwrap_or_default();
    let audio = spawn_audio();
    {
        // Persist synchronously before quitting, and only then let the 5s
        // force-exit fallback proceed.
        let player_close = player.clone();
        let audio_close = audio.clone();
        let generation_close = generation.clone();
        ui.window().on_close_requested(move || {
            save_before_exit(&player_close, &audio_close, &generation_close);
            schedule_forced_exit();
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        });
        let player_quit = player.clone();
        let audio_quit = audio.clone();
        let generation_quit = generation.clone();
        ui.on_quit_app(move || {
            save_before_exit(&player_quit, &audio_quit, &generation_quit);
            schedule_forced_exit();
            let _ = slint::quit_event_loop();
        });
    }
    {
        // Periodic checkpoint, matching Tauri's 5s persistence. This also
        // covers a crash/force-kill without writing position 0 on every
        // track switch.
        let player = player.clone();
        let audio = audio.clone();
        let generation = generation.clone();
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, Duration::from_secs(5), move || {
            if !player
                .lock()
                .map(|player| player.queue.is_empty())
                .unwrap_or(true)
            {
                let position = current_position(&audio, &player, &generation);
                save_player(&player, position);
            }
        });
        std::mem::forget(timer);
    }
    {
        // Keep the Windows media flyout's timeline in sync without flooding
        // the SMTC worker: once per second is enough for its progress bar.
        let weak = weak.clone();
        let audio = audio.clone();
        let generation = generation.clone();
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
            let Some(ui) = weak.upgrade() else { return };
            let token = generation.load(Ordering::Relaxed);
            if let Ok(snapshot) = audio.snapshot.lock() {
                if snapshot.token == token && (snapshot.loaded || snapshot.ended) {
                    let duration = if snapshot.duration > 0.5 {
                        snapshot.duration
                    } else {
                        ui.get_duration()
                    };
                    if duration > 0.5 {
                        media::update_position(snapshot.position, duration);
                    }
                }
            }
        });
        std::mem::forget(timer);
    }
    ui.set_volume(snapshot.volume);
    ui.set_quality(snapshot.quality.clone().into());
    ui.set_low_power(snapshot.low_power);
    ui.set_show_translation(snapshot.show_translation);
    ui.set_cover_accent(snapshot.cover_accent);
    ui.set_dark_mode(snapshot.theme != "light");
    let _ = audio.tx.send(AudioCmd::Volume(snapshot.volume));
    ui.set_data_dir(paths::data_dir().display().to_string().into());
    ui.set_ambient_background(ambient_background());
    ui.set_daily_feature(PlaylistItem {
        id: "daily".into(),
        title: "每日推荐".into(),
        meta: "登录后同步每日推荐".into(),
        cover: Image::default(),
        tint: Color::from_rgb_u8(201, 32, 39),
        pinned: false,
    });
    ui.set_feature_playlist(PlaylistItem {
        id: "".into(),
        title: "发现音乐".into(),
        meta: "登录后获取推荐歌单".into(),
        cover: Image::default(),
        tint: Color::from_rgb_u8(60, 40, 80),
        pinned: false,
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
                if let Some(ui) = &ui {
                    ui.set_window_minimized(true);
                }
                memory::trim_working_set("minimize", "info");
            }
            if WINDOW_BACKGROUNDED.swap(false, Ordering::Relaxed) {
                if ui.as_ref().is_some_and(|ui| !ui.get_playing()) {
                    memory::trim_working_set("background", "debug");
                }
            }
            if WINDOW_RESTORED.swap(false, Ordering::Relaxed) {
                if let Some(ui) = &ui {
                    ui.set_window_minimized(false);
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
        logger::log(
            "info",
            "restore",
            &format!(
                "id={} title={} index={} position={resume_position:.1}s",
                track.id, track.name, saved.index
            ),
        );
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
        set_media_playing(false);
        let cover_endpoint = cover_url(&track.cover_url, 500);
        if !cover_endpoint.is_empty() {
            let cover_state = state.clone();
            let cover_ui = weak.clone();
            let media_title = track.name.clone();
            let media_artist = track.artist.clone();
            let media_album = track.album.clone();
            let restored_id = track.id.clone();
            let prefs = prefs.clone();
            let restore_generation = generation.clone();
            let restore_request = generation.load(Ordering::Relaxed);
            runtime.spawn(async move {
                if let Some(path) = fetch_cover(&cover_state, cover_endpoint).await {
                    if restore_generation.load(Ordering::Relaxed) != restore_request {
                        return;
                    }
                    if let Ok(mut slot) = current_art().lock() {
                        *slot = Some((restored_id.clone(), path.clone()));
                    }
                    media::update_track(&media_title, &media_artist, &media_album, Some(&path));
                    let prefs_snapshot =
                        prefs.lock().map(|prefs| prefs.clone()).unwrap_or_default();
                    let image_path = path.clone();
                    let image = tokio::task::spawn_blocking(move || cover_buffer(&image_path))
                        .await
                        .ok()
                        .flatten();
                    let backdrop = if !prefs_snapshot.low_power && prefs_snapshot.cover_accent {
                        let backdrop_path = path;
                        tokio::task::spawn_blocking(move || blurred_backdrop_buffer(&backdrop_path))
                            .await
                            .ok()
                            .flatten()
                    } else {
                        None
                    };
                    update_ui(&cover_ui, move |ui| {
                        if ui.get_current_id().as_str() != restored_id.as_str() {
                            return;
                        }
                        if let Some(image) = image {
                            ui.set_current_cover_image(Image::from_rgba8(image));
                        }
                        if let Some(backdrop) = backdrop {
                            ui.set_current_backdrop(Image::from_rgba8(backdrop));
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
                                            ui.set_cookie_text("".into());
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
        let qr_generation = qr_generation.clone();
        ui.on_import_cookie(move |cookie| {
            let runtime = runtime.clone();
            let task_runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let qr_generation = qr_generation.clone();
            qr_generation.fetch_add(1, Ordering::Relaxed);
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
                            ui.set_cookie_text("".into());
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
        let qr_generation = qr_generation.clone();
        ui.on_close_login(move || {
            // Stop the in-flight QR poll loop; otherwise it keeps running
            // behind the closed dialog and can log the user in unexpectedly.
            qr_generation.fetch_add(1, Ordering::Relaxed);
            if let Some(ui) = weak.upgrade() {
                ui.set_login_open(false);
                ui.set_cookie_text("".into());
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let audio = audio.clone();
        let player = player.clone();
        let generation = generation.clone();
        let search_generation = search_generation.clone();
        let detail_generation = detail_generation.clone();
        let detail_history = detail_history.clone();
        let player_anim = player_anim.clone();
        ui.on_logout(move || {
            let runtime = runtime.clone();
            let task_runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let audio = audio.clone();
            let player = player.clone();
            let generation = generation.clone();
            let search_generation = search_generation.clone();
            let detail_generation = detail_generation.clone();
            let detail_history = detail_history.clone();
            let player_anim = player_anim.clone();
            runtime.spawn(async move {
                player_anim.fetch_add(1, Ordering::Relaxed);
                let logout_result = auth::logout(&state).await;
                let request = generation.fetch_add(1, Ordering::Relaxed) + 1;
                search_generation.fetch_add(1, Ordering::Relaxed);
                detail_generation.fetch_add(1, Ordering::Relaxed);
                let _ = audio.tx.send(AudioCmd::Stop { token: request });
                if let Ok(mut player) = player.lock() {
                    player.queue.clear();
                    player.index = 0;
                    player.current = None;
                    player.resume_position = 0.0;
                }
                if let Ok(mut slot) = current_art().lock() {
                    *slot = None;
                }
                if let Ok(mut catalog) = catalog.lock() {
                    catalog.detail.clear();
                    catalog.search_songs.clear();
                    catalog.search_playlists.clear();
                    catalog.search_lyric_tracks.clear();
                    catalog.library_playlists.clear();
                    catalog.library_recent.clear();
                }
                let history_clear = detail_history.clone();
                if let Ok(mut history) = history_clear.lock() {
                    history.clear();
                }
                let _ = fs::remove_file(paths::data_dir().join("last-playback.json"));
                media::clear();
                set_media_playing(false);
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
                    ui.set_current_id("".into());
                    ui.set_current_title("".into());
                    ui.set_current_artist("".into());
                    ui.set_current_album("".into());
                    ui.set_current_quality("".into());
                    ui.set_current_artists(ModelRc::from(Rc::new(VecModel::from(Vec::<
                        ArtistLinkItem,
                    >::new(
                    )))));
                    ui.set_current_cover_image(Image::default());
                    ui.set_current_backdrop(Image::default());
                    ui.set_lyrics(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<LyricItem>::new(),
                    ))));
                    ui.set_lyric_index(0);
                    ui.set_playing(false);
                    ui.set_loading(false);
                    ui.set_progress(0.0);
                    ui.set_duration(0.0);
                    ui.set_position_text("00:00".into());
                    ui.set_duration_text("00:00".into());
                    ui.set_queue_tracks(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<TrackItem>::new(),
                    ))));
                    ui.set_queue_index(0);
                    ui.set_queue_open(false);
                    ui.set_player_open(false);
                    ui.set_player_visible(false);
                    ui.set_cover_flying(false);
                    ui.set_workspace_hidden(false);
                    ui.set_controls_ready(true);
                    ui.set_detail_kind("".into());
                    ui.set_detail_id("".into());
                    ui.set_detail_title("".into());
                    ui.set_detail_meta("".into());
                    ui.set_detail_description("".into());
                    ui.set_detail_first_id("".into());
                    ui.set_detail_tracks(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<TrackItem>::new(),
                    ))));
                    ui.set_detail_albums(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<PlaylistItem>::new(),
                    ))));
                    ui.set_library_playlists(ModelRc::from(Rc::new(VecModel::from(Vec::<
                        PlaylistItem,
                    >::new(
                    )))));
                    ui.set_library_tracks(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<TrackItem>::new(),
                    ))));
                    ui.set_library_playlists_expanded(false);
                    ui.set_library_common_count(8);
                    ui.set_library_playlists_total(0);
                    ui.set_search_text("".into());
                    ui.set_search_query("".into());
                    ui.set_search_loading(false);
                    ui.set_search_tracks(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<TrackItem>::new(),
                    ))));
                    ui.set_search_playlists(ModelRc::from(Rc::new(VecModel::from(Vec::<
                        PlaylistItem,
                    >::new(
                    )))));
                    ui.set_search_artists(ModelRc::from(Rc::new(VecModel::from(Vec::<
                        PlaylistItem,
                    >::new(
                    )))));
                    ui.set_search_albums(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<PlaylistItem>::new(),
                    ))));
                    ui.set_search_lyrics(ModelRc::from(Rc::new(VecModel::from(Vec::<
                        LyricResultItem,
                    >::new(
                    )))));
                    ui.set_recent_tracks(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<TrackItem>::new(),
                    ))));
                    ui.set_daily_tracks(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<TrackItem>::new(),
                    ))));
                    ui.set_new_tracks(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<TrackItem>::new(),
                    ))));
                    ui.set_playlists(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<PlaylistItem>::new(),
                    ))));
                    ui.set_daily_feature(PlaylistItem {
                        id: "daily".into(),
                        title: "每日推荐".into(),
                        meta: "登录后同步每日推荐".into(),
                        cover: Image::default(),
                        tint: Color::from_rgb_u8(201, 32, 39),
                        pinned: false,
                    });
                    ui.set_feature_playlist(PlaylistItem {
                        id: "".into(),
                        title: "发现音乐".into(),
                        meta: "登录后获取推荐歌单".into(),
                        cover: Image::default(),
                        tint: Color::from_rgb_u8(60, 40, 80),
                        pinned: false,
                    });
                    switch_page(ui, "home");
                    ui.set_status_text(
                        if logout_result.is_ok() {
                            "已退出登录"
                        } else {
                            "已退出登录（本地凭证清理失败）"
                        }
                        .into(),
                    );
                    ui.invoke_refresh_qr();
                });
                // Rebuild the home page from public data; no previous-account
                // personal playlists/history remain in the UI.
                load_home(&task_runtime, state, weak, catalog);
            });
        });
    }

    {
        let weak = weak.clone();
        let runtime = runtime.clone();
        let state = state.clone();
        let catalog = catalog.clone();
        let detail_history = detail_history.clone();
        let search_generation = search_generation.clone();
        let detail_generation = detail_generation.clone();
        ui.on_nav(move |page| {
            let page = page.to_string();
            // Navigating away cancels any in-flight search/detail response so
            // it cannot force the old page back.
            search_generation.fetch_add(1, Ordering::Relaxed);
            detail_generation.fetch_add(1, Ordering::Relaxed);
            if let Some(mut history) = detail_history.lock().ok() {
                history.clear();
            }
            let logged_in = weak.upgrade().map(|ui| ui.get_logged_in()).unwrap_or(false);
            if let Some(ui) = weak.upgrade() {
                switch_page(&ui, &page);
            }
            if page == "library" && logged_in {
                load_library(&runtime, state.clone(), weak.clone(), catalog.clone());
            }
            if page == "home" {
                load_home(&runtime, state.clone(), weak.clone(), catalog.clone());
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let search_generation = search_generation.clone();
        let detail_generation = detail_generation.clone();
        let detail_history = detail_history.clone();
        ui.on_search(move |keyword, kind| {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let search_generation = search_generation.clone();
            let detail_generation = detail_generation.clone();
            let detail_history = detail_history.clone();
            detail_generation.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut history) = detail_history.lock() {
                history.clear();
            }
            let request = search_generation.fetch_add(1, Ordering::Relaxed) + 1;
            let keyword = keyword.to_string();
            let kind = kind.to_string();
            let lyric_search = kind == "lyric";
            let lyric_keyword = keyword.clone();
            if keyword.trim().is_empty() {
                update_ui(&weak, move |ui| {
                    switch_page(ui, "search");
                    ui.set_search_query("".into());
                    ui.set_search_loading(false);
                    ui.set_search_tracks(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<TrackItem>::new(),
                    ))));
                    ui.set_search_playlists(ModelRc::from(Rc::new(VecModel::from(Vec::<
                        PlaylistItem,
                    >::new(
                    )))));
                    ui.set_search_artists(ModelRc::from(Rc::new(VecModel::from(Vec::<
                        PlaylistItem,
                    >::new(
                    )))));
                    ui.set_search_albums(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<PlaylistItem>::new(),
                    ))));
                    ui.set_search_lyrics(ModelRc::from(Rc::new(VecModel::from(Vec::<
                        LyricResultItem,
                    >::new(
                    )))));
                    ui.set_status_text("".into());
                });
                return;
            }
            let display_keyword = keyword.clone();
            let wait_for_lyric = lyric_search;
            runtime.spawn(async move {
                update_ui(&weak, move |ui| {
                    switch_page(ui, "search");
                    ui.set_search_query(display_keyword.into());
                    ui.set_search_loading(true);
                    ui.set_search_tracks(ModelRc::from(Rc::new(VecModel::from(
                        Vec::<TrackItem>::new(),
                    ))));
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
                        let search_error = data.error.clone();
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
                            if !wait_for_lyric {
                                ui.set_search_loading(false);
                            }
                            let _ = data.songs.len();
                            if let Some(error) = search_error {
                                ui.set_status_text(error.into());
                            } else {
                                ui.set_status_text("".into());
                            }
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
                        if lyric_search {
                            let lyric_state = state.clone();
                            let lyric_ui = weak.clone();
                            let lyric_generation = search_generation.clone();
                            let lyric_catalog = catalog.clone();
                            tokio::spawn(async move {
                                let result = netease::search_kind(
                                    &lyric_state,
                                    "lyric".to_string(),
                                    lyric_keyword,
                                )
                                .await;
                                if lyric_generation.load(Ordering::Relaxed) != request {
                                    return;
                                }
                                match result {
                                    Ok(data) => {
                                        if let Ok(mut catalog) = lyric_catalog.lock() {
                                            catalog.search_lyric_tracks = data
                                                .lyrics
                                                .iter()
                                                .map(|item| item.track.clone())
                                                .collect();
                                        }
                                        update_ui(&lyric_ui, move |ui| {
                                            ui.set_search_lyrics(ModelRc::from(Rc::new(
                                                VecModel::from(lyric_result_items_for_ui(
                                                    &data.lyrics,
                                                )),
                                            )));
                                            ui.set_search_loading(false);
                                        });
                                    }
                                    Err(error) => update_ui(&lyric_ui, move |ui| {
                                        ui.set_search_loading(false);
                                        ui.set_search_lyrics(ModelRc::from(Rc::new(
                                            VecModel::from(Vec::<LyricResultItem>::new()),
                                        )));
                                        ui.set_status_text(error.into());
                                    }),
                                }
                            });
                        }
                    }
                    Err(error) => {
                        if search_generation.load(Ordering::Relaxed) != request {
                            return;
                        }
                        update_ui(&weak, move |ui| {
                            ui.set_search_loading(false);
                            // Hiding the old query prevents stale results and
                            // a misleading "no results" state on failure.
                            ui.set_search_query("".into());
                            ui.set_status_text(error.into());
                        });
                    }
                }
            });
        });
    }

    {
        let weak = weak.clone();
        let catalog = catalog.clone();
        ui.on_toggle_playlist_pin(move |id| {
            let id = id.to_string();
            let mut pinned_now = false;
            if let Ok(mut state) = playlist_state().lock() {
                if let Some(position) = state.pinned.iter().position(|item| item == &id) {
                    state.pinned.remove(position);
                } else {
                    state.pinned.push(id.clone());
                    pinned_now = true;
                }
                save_playlist_state(&state);
            }
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let playlists = catalog
                .lock()
                .map(|catalog| catalog.library_playlists.clone())
                .unwrap_or_default();
            let ordered = ordered_library_playlists(&playlists);
            let state_snapshot = playlist_state()
                .lock()
                .map(|state| state.clone())
                .unwrap_or_default();
            let mut covers = std::collections::HashMap::<String, Image>::new();
            let current = ui.get_library_playlists();
            for index in 0..current.row_count() {
                if let Some(item) = current.row_data(index) {
                    covers.insert(item.id.to_string(), item.cover.clone());
                }
            }
            let mut items = ordered
                .iter()
                .map(|playlist| PlaylistItem {
                    id: playlist.id.clone().into(),
                    title: playlist.name.clone().into(),
                    meta: format!("{} · {} 首", playlist.creator, playlist.track_count).into(),
                    cover: covers.remove(&playlist.id).unwrap_or_default(),
                    tint: color_for(&playlist.id),
                    pinned: playlist_is_pinned(&state_snapshot, &playlist.id),
                })
                .collect::<Vec<_>>();
            if !ui.get_library_playlists_expanded() {
                let common = playlist_common_count(&ordered).max(0) as usize;
                items.truncate(common);
            }
            if let Ok(mut catalog) = catalog.lock() {
                catalog.library_playlists = ordered.clone();
            }
            ui.set_library_playlists(ModelRc::from(Rc::new(VecModel::from(items))));
            ui.set_library_common_count(playlist_common_count(&ordered));
            ui.set_library_playlists_total(ordered.len() as i32);
            set_status_temp(
                &ui,
                if pinned_now {
                    "已置顶歌单".to_string()
                } else {
                    "已取消置顶".to_string()
                },
                Duration::from_millis(2000),
            );
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        ui.on_toggle_library_playlists(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let expanded = !ui.get_library_playlists_expanded();
            ui.set_library_playlists_expanded(expanded);
            let playlists = catalog
                .lock()
                .map(|catalog| catalog.library_playlists.clone())
                .unwrap_or_default();
            let ordered = ordered_library_playlists(&playlists);
            ui.set_library_playlists(ModelRc::from(Rc::new(VecModel::from(
                library_visible_playlists(&ordered, expanded),
            ))));
            ui.set_library_common_count(playlist_common_count(&ordered));
            ui.set_library_playlists_total(ordered.len() as i32);
            if expanded {
                runtime.spawn(load_playlist_covers(
                    state.clone(),
                    weak.clone(),
                    PlaylistTarget::Library,
                    ordered,
                    200,
                ));
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
        let detail_generation = detail_generation.clone();
        ui.on_open_playlist(move |id| {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let prefs = prefs.clone();
            let detail_history = detail_history.clone();
            let detail_generation = detail_generation.clone();
            let id = id.to_string();
            if let Ok(mut usage) = playlist_state().lock() {
                *usage.usage.entry(id.clone()).or_insert(0) += 1;
                save_playlist_state(&usage);
            }
            if let Some(ui) = weak.upgrade() {
                let page = ui.get_page().to_string();
                let existing_origin = ui.get_detail_origin().to_string();
                let origin = if page == "detail" {
                    if existing_origin.is_empty() {
                        "home".to_string()
                    } else {
                        existing_origin
                    }
                } else {
                    page
                };
                ui.set_detail_origin(origin.into());
            }
            if let Ok(mut history) = detail_history.lock() {
                history.clear();
            }
            let request = detail_generation.fetch_add(1, Ordering::Relaxed) + 1;
            runtime.spawn(async move {
                match netease::playlist_detail(&state, id).await {
                    Ok(playlist) => {
                        if detail_generation.load(Ordering::Relaxed) != request {
                            return;
                        }
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
                            200,
                        ));
                        if !cover_endpoint.is_empty() {
                            let cover_state = state.clone();
                            let cover_ui = weak.clone();
                            let cover_id = playlist.id.clone();
                            let cover_generation = detail_generation.clone();
                            tokio::spawn(async move {
                                if let Some(path) = tokio::time::timeout(
                                    Duration::from_secs(8),
                                    fetch_cover(&cover_state, cover_endpoint),
                                )
                                .await
                                .ok()
                                .flatten()
                                {
                                    if cover_generation.load(Ordering::Relaxed) != request {
                                        return;
                                    }
                                    let prefs_snapshot =
                                        prefs.lock().map(|prefs| prefs.clone()).unwrap_or_default();
                                    let backdrop = if !prefs_snapshot.low_power
                                        && prefs_snapshot.cover_accent
                                    {
                                        let backdrop_path = path;
                                        tokio::task::spawn_blocking(move || {
                                            blurred_backdrop_buffer(&backdrop_path)
                                        })
                                        .await
                                        .ok()
                                        .flatten()
                                    } else {
                                        None
                                    };
                                    let Some(backdrop) = backdrop else {
                                        return;
                                    };
                                    update_ui(&cover_ui, move |ui| {
                                        if ui.get_detail_kind().as_str() != "playlist"
                                            || ui.get_detail_id().as_str() != cover_id.as_str()
                                        {
                                            return;
                                        }
                                        ui.set_detail_backdrop(Image::from_rgba8(backdrop));
                                    });
                                }
                            });
                        }
                    }
                    Err(error) => {
                        if detail_generation.load(Ordering::Relaxed) != request {
                            return;
                        }
                        update_ui(&weak, move |ui| ui.set_status_text(error.into()));
                    }
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
        let detail_generation = detail_generation.clone();
        ui.on_open_artist(move |id| {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let prefs = prefs.clone();
            let detail_history = detail_history.clone();
            let detail_generation = detail_generation.clone();
            let id = id.to_string();
            if let Some(ui) = weak.upgrade() {
                if ui.get_page().as_str() == "detail" && ui.get_detail_kind().as_str() == "artist" {
                    if let Ok(mut history) = detail_history.lock() {
                        history.push(("artist".to_string(), ui.get_detail_id().to_string()));
                    }
                }
                let page = ui.get_page().to_string();
                let existing_origin = ui.get_detail_origin().to_string();
                let origin = if page == "detail" {
                    if existing_origin.is_empty() {
                        "home".to_string()
                    } else {
                        existing_origin
                    }
                } else {
                    page
                };
                ui.set_detail_origin(origin.into());
            }
            let request = detail_generation.fetch_add(1, Ordering::Relaxed) + 1;
            runtime.spawn(async move {
                match netease::artist_page(&state, id.clone()).await {
                    Ok(data) => {
                        if detail_generation.load(Ordering::Relaxed) != request {
                            return;
                        }
                        let album_error = data.album_error.clone();
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
                            ui.set_detail_backdrop(Image::default());
                            ui.set_detail_albums(ModelRc::from(Rc::new(VecModel::from(
                                album_items_for_ui(&albums),
                            ))));
                            ui.set_detail_tracks(ModelRc::from(Rc::new(VecModel::from(
                                track_items_for_ui(&songs_for_ui),
                            ))));
                            if let Some(error) = album_error {
                                set_status_temp(
                                    ui,
                                    format!("专辑加载失败：{error}"),
                                    Duration::from_millis(3200),
                                );
                            }
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
                            let cover_generation = detail_generation.clone();
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
                                    if cover_generation.load(Ordering::Relaxed) != request {
                                        return;
                                    }
                                    let prefs_snapshot =
                                        prefs.lock().map(|prefs| prefs.clone()).unwrap_or_default();
                                    let backdrop = if !prefs_snapshot.low_power
                                        && prefs_snapshot.cover_accent
                                    {
                                        let backdrop_path = path;
                                        tokio::task::spawn_blocking(move || {
                                            blurred_backdrop_buffer(&backdrop_path)
                                        })
                                        .await
                                        .ok()
                                        .flatten()
                                    } else {
                                        None
                                    };
                                    let Some(backdrop) = backdrop else {
                                        return;
                                    };
                                    update_ui(&cover_ui, move |ui| {
                                        if ui.get_detail_kind().as_str() != "artist"
                                            || ui.get_detail_id().as_str() != expected_id.as_str()
                                        {
                                            return;
                                        }
                                        ui.set_detail_backdrop(Image::from_rgba8(backdrop));
                                    });
                                }
                            });
                        }
                    }
                    Err(error) => {
                        if detail_generation.load(Ordering::Relaxed) != request {
                            return;
                        }
                        update_ui(&weak, move |ui| ui.set_status_text(error.into()));
                    }
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
        let detail_generation = detail_generation.clone();
        ui.on_open_album(move |id| {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let prefs = prefs.clone();
            let detail_history = detail_history.clone();
            let detail_generation = detail_generation.clone();
            let id = id.to_string();
            let previous_artist = if let Some(ui) = weak.upgrade() {
                if ui.get_page().as_str() == "detail" && ui.get_detail_kind().as_str() == "artist" {
                    Some(ui.get_detail_id().to_string())
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(ui) = weak.upgrade() {
                let page = ui.get_page().to_string();
                let existing_origin = ui.get_detail_origin().to_string();
                let origin = if page == "detail" {
                    if existing_origin.is_empty() {
                        "home".to_string()
                    } else {
                        existing_origin
                    }
                } else {
                    page
                };
                ui.set_detail_origin(origin.into());
            }
            let history_for_task = detail_history.clone();
            let request = detail_generation.fetch_add(1, Ordering::Relaxed) + 1;
            runtime.spawn(async move {
                match netease::album_page(id.clone()).await {
                    Ok(data) => {
                        if detail_generation.load(Ordering::Relaxed) != request {
                            return;
                        }
                        // Only record the artist page after navigation
                        // succeeded; a failed album load must not pollute the
                        // back stack with the current page itself.
                        if let Some(artist_id) = previous_artist {
                            if let Ok(mut history) = history_for_task.lock() {
                                history.push(("artist".to_string(), artist_id));
                            }
                        }
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
                            let cover_generation = detail_generation.clone();
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
                                    if cover_generation.load(Ordering::Relaxed) != request {
                                        return;
                                    }
                                    let prefs_snapshot =
                                        prefs.lock().map(|prefs| prefs.clone()).unwrap_or_default();
                                    let backdrop = if !prefs_snapshot.low_power
                                        && prefs_snapshot.cover_accent
                                    {
                                        let backdrop_path = path;
                                        tokio::task::spawn_blocking(move || {
                                            blurred_backdrop_buffer(&backdrop_path)
                                        })
                                        .await
                                        .ok()
                                        .flatten()
                                    } else {
                                        None
                                    };
                                    let Some(backdrop) = backdrop else {
                                        return;
                                    };
                                    update_ui(&cover_ui, move |ui| {
                                        if ui.get_detail_kind().as_str() != "album"
                                            || ui.get_detail_id().as_str() != expected_id.as_str()
                                        {
                                            return;
                                        }
                                        ui.set_detail_backdrop(Image::from_rgba8(backdrop));
                                    });
                                }
                            });
                        }
                    }
                    Err(error) => {
                        if detail_generation.load(Ordering::Relaxed) != request {
                            return;
                        }
                        update_ui(&weak, move |ui| ui.set_status_text(error.into()));
                    }
                }
            });
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let detail_history = detail_history.clone();
        let detail_generation = detail_generation.clone();
        ui.on_back_detail(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            detail_generation.fetch_add(1, Ordering::Relaxed);
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
                let origin = ui.get_detail_origin().to_string();
                ui.set_detail_kind("".into());
                switch_page(&ui, if origin.is_empty() { "search" } else { &origin });
            } else if kind == "playlist" {
                let origin = ui.get_detail_origin().to_string();
                ui.set_detail_kind("".into());
                switch_page(&ui, if origin.is_empty() { "home" } else { &origin });
                if origin == "library" {
                    load_library(&runtime, state.clone(), weak.clone(), catalog.clone());
                }
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
        let detail_generation = detail_generation.clone();
        ui.on_open_daily(move || {
            let request = detail_generation.fetch_add(1, Ordering::Relaxed) + 1;
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
                200,
            ));
            if let Some(track) = first {
                let cover_endpoint = cover_url(&track.cover_url, 500);
                if !cover_endpoint.is_empty() {
                    let cover_state = state.clone();
                    let cover_ui = weak.clone();
                    let cover_prefs = prefs.clone();
                    let cover_generation = detail_generation.clone();
                    runtime.spawn(async move {
                        if let Some(path) = tokio::time::timeout(
                            Duration::from_secs(8),
                            fetch_cover(&cover_state, cover_endpoint),
                        )
                        .await
                        .ok()
                        .flatten()
                        {
                            if cover_generation.load(Ordering::Relaxed) != request {
                                return;
                            }
                            let prefs_snapshot = cover_prefs
                                .lock()
                                .map(|prefs| prefs.clone())
                                .unwrap_or_default();
                            let backdrop =
                                if !prefs_snapshot.low_power && prefs_snapshot.cover_accent {
                                    let backdrop_path = path;
                                    tokio::task::spawn_blocking(move || {
                                        blurred_backdrop_buffer(&backdrop_path)
                                    })
                                    .await
                                    .ok()
                                    .flatten()
                                } else {
                                    None
                                };
                            let Some(backdrop) = backdrop else {
                                return;
                            };
                            update_ui(&cover_ui, move |ui| {
                                if ui.get_page().as_str() != "detail"
                                    || ui.get_detail_kind().as_str() != "daily"
                                {
                                    return;
                                }
                                ui.set_detail_backdrop(Image::from_rgba8(backdrop));
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
        ui.on_play_track(move |id, context| {
            let id = id.to_string();
            let context = context.to_string();
            let resolved = catalog
                .lock()
                .ok()
                .and_then(|catalog| find_track_in_context(&catalog, &context, &id))
                .or_else(|| {
                    let local = local_recent().lock().ok()?.clone();
                    local
                        .iter()
                        .find(|track| track.id == id)
                        .cloned()
                        .map(|track| (track, local))
                })
                .or_else(|| {
                    catalog
                        .lock()
                        .ok()
                        .and_then(|catalog| find_track(&catalog, &id))
                });
            let Some((track, context)) = resolved else {
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
                true,
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
                true,
            );
        });
    }

    {
        let runtime = runtime.clone();
        let weak = weak.clone();
        let prefs = prefs.clone();
        ui.on_set_low_power(move |value| {
            if let Ok(mut locked) = prefs.lock() {
                locked.low_power = value;
                save_prefs(&locked);
            }
            let Some(ui) = weak.upgrade() else {
                return;
            };
            ui.set_low_power(value);
            // Do not clear the already generated backdrop: if the user
            // turns performance mode off again, the current track must
            // regain its ambience without waiting for the next track.
            set_status_temp(
                &ui,
                if value {
                    "已开启性能模式：暂停生成封面模糊".to_string()
                } else {
                    "已关闭性能模式".to_string()
                },
                Duration::from_millis(2400),
            );
            if value {
                return;
            }
            let accent = prefs.lock().map(|prefs| prefs.cover_accent).unwrap_or(true);
            if !accent {
                return;
            }
            let art = current_art().lock().ok().and_then(|slot| slot.clone());
            if let Some((id, path)) = art {
                let ui_weak = ui.as_weak();
                runtime.spawn(async move {
                    let buffer =
                        tokio::task::spawn_blocking(move || blurred_backdrop_buffer(&path))
                            .await
                            .ok()
                            .flatten();
                    let Some(buffer) = buffer else { return };
                    update_ui(&ui_weak, move |ui| {
                        if ui.get_current_id().as_str() == id.as_str() {
                            ui.set_current_backdrop(Image::from_rgba8(buffer));
                        }
                    });
                });
            }
        });
    }

    {
        let weak = weak.clone();
        let prefs = prefs.clone();
        ui.on_set_cover_accent(move |value| {
            if let Ok(mut locked) = prefs.lock() {
                locked.cover_accent = value;
                save_prefs(&locked);
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_cover_accent(value);
                set_status_temp(
                    &ui,
                    if value {
                        "已显示封面氛围背景".to_string()
                    } else {
                        "已隐藏封面氛围背景".to_string()
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
            if let Ok(mut locked) = prefs.lock() {
                locked.show_translation = value;
                save_prefs(&locked);
            }
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
                true,
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
            if !ui.get_logged_in() {
                set_status_temp(
                    &ui,
                    "登录后才能喜欢歌曲".to_string(),
                    Duration::from_millis(2400),
                );
                return;
            }
            let id = ui.get_current_id().to_string();
            if id.is_empty() {
                return;
            }
            let already_pending = like_inflight()
                .lock()
                .map(|mut set| !set.insert(id.clone()))
                .unwrap_or(false);
            if already_pending {
                return;
            }
            let liked = ui.get_current_liked();
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let epoch = state.session_epoch();
            runtime.spawn(async move {
                let result = netease::like_song(&state, id.clone(), !liked).await;
                if let Ok(mut set) = like_inflight().lock() {
                    set.remove(&id);
                }
                if state.session_epoch() != epoch {
                    return;
                }
                match result {
                    Ok(result) => {
                        if let Ok(mut set) = liked_ids().lock() {
                            if result.liked {
                                set.insert(id.clone());
                            } else {
                                set.remove(&id);
                            }
                        }
                        update_ui(&weak, move |ui| {
                            // A slow response must not overwrite the state of a
                            // song the user has switched to in the meantime.
                            if ui.get_current_id().as_str() == id.as_str() {
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
        ui.on_toggle_subscribe(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !ui.get_logged_in() {
                set_status_temp(
                    &ui,
                    "登录后才能收藏歌单".to_string(),
                    Duration::from_millis(2400),
                );
                return;
            }
            let id = ui.get_detail_id().to_string();
            if id.is_empty() {
                return;
            }
            let already_pending = subscribe_inflight()
                .lock()
                .map(|mut set| !set.insert(id.clone()))
                .unwrap_or(false);
            if already_pending {
                return;
            }
            let subscribed = ui.get_detail_subscribed();
            let runtime = runtime.clone();
            let task_runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let epoch = state.session_epoch();
            runtime.spawn(async move {
                let result = netease::subscribe_playlist(id.clone(), !subscribed).await;
                if let Ok(mut set) = subscribe_inflight().lock() {
                    set.remove(&id);
                }
                if state.session_epoch() != epoch {
                    return;
                }
                match result {
                    Ok(()) => {
                        update_ui(&weak, move |ui| {
                            if ui.get_detail_id().as_str() == id.as_str() {
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
                            }
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
            if !ui.get_logged_in() {
                set_status_temp(
                    &ui,
                    "登录后才能管理歌单".to_string(),
                    Duration::from_millis(2400),
                );
                return;
            }
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
        let generation = generation.clone();
        ui.on_seek(move |value| {
            let token = generation.load(Ordering::Relaxed);
            let loaded = audio
                .snapshot
                .lock()
                .map(|snapshot| snapshot.loaded && snapshot.token == token)
                .unwrap_or(false);
            if !loaded {
                if let Ok(mut player) = player.lock() {
                    player.resume_position = value.max(0.0);
                }
            }
            let _ = audio.tx.send(AudioCmd::Seek { value, token });
            if let Some(ui) = weak.upgrade() {
                ui.set_progress(value);
                ui.set_position_text(format_clock(value).into());
            }
        });
    }

    {
        let audio = audio.clone();
        let weak = weak.clone();
        let player = player.clone();
        let generation = generation.clone();
        ui.on_seek_lyric(move |time| {
            let token = generation.load(Ordering::Relaxed);
            let loaded = audio
                .snapshot
                .lock()
                .map(|snapshot| snapshot.loaded && snapshot.token == token)
                .unwrap_or(false);
            if !loaded {
                if let Ok(mut player) = player.lock() {
                    player.resume_position = time.max(0.0);
                }
            }
            let _ = audio.tx.send(AudioCmd::Seek { value: time, token });
            if let Some(ui) = weak.upgrade() {
                ui.set_progress(time);
                ui.set_position_text(format_clock(time).into());
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
        let weak = weak.clone();
        let prefs = prefs.clone();
        ui.on_set_theme(move |value| {
            let theme = if value.as_str() == "light" {
                "light"
            } else {
                "dark"
            };
            if let Ok(mut prefs) = prefs.lock() {
                prefs.theme = theme.to_string();
                save_prefs(&prefs);
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_dark_mode(theme == "dark");
            }
        });
    }

    {
        let weak = weak.clone();
        ui.on_copy_song_info(move |text| {
            if let Some(ui) = weak.upgrade() {
                let copied = media::copy_text(text.as_str());
                set_status_temp(
                    &ui,
                    if copied {
                        "已复制歌曲信息".to_string()
                    } else {
                        "复制失败，请检查剪贴板".to_string()
                    },
                    Duration::from_millis(2200),
                );
            }
        });
    }

    {
        let weak = weak.clone();
        ui.on_copy_detail_link(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let kind = ui.get_detail_kind().to_string();
            let id = ui.get_detail_id().to_string();
            let Some(url) = detail_share_url(&kind, &id) else {
                set_status_temp(
                    &ui,
                    "当前页面没有可分享的链接".to_string(),
                    Duration::from_millis(2200),
                );
                return;
            };
            let copied = media::copy_text(&url);
            set_status_temp(
                &ui,
                if copied {
                    "已复制链接".to_string()
                } else {
                    "复制失败，请检查剪贴板".to_string()
                },
                Duration::from_millis(2200),
            );
        });
    }

    {
        let weak = weak.clone();
        ui.on_open_detail_link(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let kind = ui.get_detail_kind().to_string();
            let id = ui.get_detail_id().to_string();
            let Some(url) = detail_share_url(&kind, &id) else {
                set_status_temp(
                    &ui,
                    "当前页面没有可打开的链接".to_string(),
                    Duration::from_millis(2200),
                );
                return;
            };
            if !open_external_url(&url) {
                set_status_temp(
                    &ui,
                    "无法打开浏览器".to_string(),
                    Duration::from_millis(2200),
                );
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        ui.on_toggle_track_like(move |id| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !ui.get_logged_in() {
                set_status_temp(
                    &ui,
                    "登录后才能喜欢歌曲".to_string(),
                    Duration::from_millis(2200),
                );
                return;
            }
            let id = id.to_string();
            if id.is_empty() {
                return;
            }
            let already_pending = like_inflight()
                .lock()
                .map(|mut set| !set.insert(id.clone()))
                .unwrap_or(false);
            if already_pending {
                return;
            }
            let liked = liked_ids()
                .lock()
                .map(|set| set.contains(&id))
                .unwrap_or(false);
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let epoch = state.session_epoch();
            runtime.spawn(async move {
                let result = netease::like_song(&state, id.clone(), !liked).await;
                if let Ok(mut set) = like_inflight().lock() {
                    set.remove(&id);
                }
                if state.session_epoch() != epoch {
                    return;
                }
                match result {
                    Ok(result) => {
                        if let Ok(mut set) = liked_ids().lock() {
                            if result.liked {
                                set.insert(id.clone());
                            } else {
                                set.remove(&id);
                            }
                        }
                        update_ui(&weak, move |ui| {
                            if ui.get_current_id().as_str() == id.as_str() {
                                ui.set_current_liked(result.liked);
                            }
                            set_status_temp(
                                &ui,
                                if result.liked {
                                    "已添加到我喜欢的音乐".to_string()
                                } else {
                                    "已取消喜欢".to_string()
                                },
                                Duration::from_millis(2200),
                            );
                        });
                    }
                    Err(error) => update_ui(&weak, move |ui| {
                        set_status_temp(&ui, error, Duration::from_millis(3200))
                    }),
                }
            });
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let prefs = prefs.clone();
        let weak = weak.clone();
        let audio = audio.clone();
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        ui.on_set_track_quality(move |value| {
            let value = value.to_string();
            let level = if value.is_empty() {
                String::new()
            } else {
                normalize_quality(&value).to_string()
            };
            let current_track_id = player
                .lock()
                .ok()
                .and_then(|player| player.current.as_ref().map(|track| track.id.clone()));
            if let (Some(track_id), Ok(mut map)) = (current_track_id, track_quality_map().lock()) {
                if level.is_empty() {
                    map.remove(&track_id);
                } else {
                    map.insert(track_id, level.clone());
                }
                save_track_quality(&map);
            }
            let Some(ui) = weak.upgrade() else {
                return;
            };
            ui.set_current_track_quality(level.clone().into());
            set_status_temp(
                &ui,
                if level.is_empty() {
                    "本曲音质已跟随全局设置".to_string()
                } else {
                    format!("本曲音质：{}", quality_label(&level))
                },
                Duration::from_millis(2200),
            );
            let Some(track) = player.lock().ok().and_then(|player| player.current.clone()) else {
                return;
            };
            let position = current_position(&audio, &player, &generation);
            if let Ok(mut player) = player.lock() {
                player.resume_position = position.max(0.0);
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
                ui.get_playing(),
            );
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        let audio = audio.clone();
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        let prefs = prefs.clone();
        ui.on_play_detail_shuffle(move || {
            use rand::seq::SliceRandom;
            let mut tracks = catalog
                .lock()
                .map(|catalog| catalog.detail.clone())
                .unwrap_or_default();
            if tracks.is_empty() {
                if let Some(ui) = weak.upgrade() {
                    set_status_temp(
                        &ui,
                        "当前列表没有可播放的歌曲".to_string(),
                        Duration::from_millis(2200),
                    );
                }
                return;
            }
            tracks.shuffle(&mut rand::thread_rng());
            let first = tracks[0].clone();
            {
                let Ok(mut player) = player.lock() else {
                    return;
                };
                player.queue = tracks;
                player.index = 0;
                player.current = Some(first.clone());
                player.resume_position = 0.0;
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
                first,
                true,
            );
            if let Some(ui) = weak.upgrade() {
                set_status_temp(&ui, "随机播放".to_string(), Duration::from_millis(1600));
            }
        });
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let prefs = prefs.clone();
        let weak = weak.clone();
        let audio = audio.clone();
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        ui.on_set_quality(move |value| {
            let quality = normalize_quality(value.as_str()).to_string();
            if let Ok(mut prefs) = prefs.lock() {
                prefs.quality = quality.clone();
                save_prefs(&prefs);
            }
            let Some(ui) = weak.upgrade() else {
                return;
            };
            ui.set_quality(quality.clone().into());
            set_status_temp(
                &ui,
                format!("播放音质：{}", quality_label(&quality)),
                Duration::from_millis(2400),
            );
            // Apply the new quality to the current track immediately. Keep
            // the current position and whether the user was playing.
            let Some(track) = player.lock().ok().and_then(|player| player.current.clone()) else {
                return;
            };
            let token = generation.load(Ordering::Relaxed);
            let position = audio
                .snapshot
                .lock()
                .ok()
                .and_then(|snapshot| {
                    if snapshot.loaded && snapshot.token == token {
                        Some(snapshot.position)
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| {
                    player
                        .lock()
                        .map(|player| player.resume_position)
                        .unwrap_or(0.0)
                });
            if let Ok(mut player) = player.lock() {
                player.resume_position = position.max(0.0);
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
                ui.get_playing(),
            );
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
                ui.set_controls_ready(false);
                ui.set_lyrics_mode(false);
                ui.set_workspace_hidden(false);
                // Let the player layout settle before the clone reads the real
                // cover/control geometry; this keeps the landing pixel-exact.
                ui.set_player_visible(true);
                let fly = ui.as_weak();
                let fly_anim = player_anim.clone();
                Timer::single_shot(Duration::from_millis(32), move || {
                    if fly_anim.load(Ordering::Relaxed) == request {
                        if let Some(ui) = fly.upgrade() {
                            ui.set_cover_flying(true);
                        }
                    }
                });
                let handle = ui.as_weak();
                let anim = player_anim.clone();
                Timer::single_shot(Duration::from_millis(48), move || {
                    if anim.load(Ordering::Relaxed) == request {
                        if let Some(ui) = handle.upgrade() {
                            ui.set_player_open(true);
                        }
                    }
                });
                let settle = ui.as_weak();
                let settle_anim = player_anim.clone();
                Timer::single_shot(Duration::from_millis(450), move || {
                    if settle_anim.load(Ordering::Relaxed) == request {
                        if let Some(ui) = settle.upgrade() {
                            ui.set_cover_flying(false);
                            ui.set_controls_ready(true);
                            ui.set_repaint_nudge(!ui.get_repaint_nudge());
                        }
                    }
                });
                // Once the player is fully opaque, stop laying out/painting the
                // page behind it. This is a large win for software rendering.
                let hide = ui.as_weak();
                let hide_anim = player_anim.clone();
                Timer::single_shot(Duration::from_millis(340), move || {
                    if hide_anim.load(Ordering::Relaxed) == request {
                        if let Some(ui) = hide.upgrade() {
                            if ui.get_player_open() {
                                ui.set_workspace_hidden(true);
                            }
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
                ui.set_workspace_hidden(false);
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
                                ui.set_controls_ready(true);
                            }
                        }
                    });
                } else {
                    ui.set_controls_ready(false);
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
                    let hide = ui.as_weak();
                    let hide_anim = player_anim.clone();
                    Timer::single_shot(Duration::from_millis(240), move || {
                        if hide_anim.load(Ordering::Relaxed) == request {
                            if let Some(ui) = hide.upgrade() {
                                // The cross-fade is 220ms; removing the panel
                                // now prevents transparent controls from
                                // intercepting clicks after the fade.
                                ui.set_player_visible(false);
                            }
                        }
                    });
                    let settle = ui.as_weak();
                    let settle_anim = player_anim.clone();
                    Timer::single_shot(Duration::from_millis(450), move || {
                        if settle_anim.load(Ordering::Relaxed) == request {
                            if let Some(ui) = settle.upgrade() {
                                ui.set_player_visible(false);
                                ui.set_cover_flying(false);
                                ui.set_lyrics_mode(false);
                                ui.set_controls_ready(true);
                                ui.set_repaint_nudge(!ui.get_repaint_nudge());
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
        let mut last_audio_error: Option<String> = None;
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
                Some(media::MediaCommand::Seek(value)) => {
                    let value = value.clamp(0.0, ui.get_duration().max(0.01));
                    let token = generation.load(Ordering::Relaxed);
                    let loaded = audio
                        .snapshot
                        .lock()
                        .map(|snapshot| snapshot.loaded && snapshot.token == token)
                        .unwrap_or(false);
                    if !loaded {
                        if let Ok(mut player) = player.lock() {
                            player.resume_position = value.max(0.0);
                        }
                    }
                    let _ = audio.tx.send(AudioCmd::Seek { value, token });
                    ui.set_progress(value);
                    ui.set_position_text(format_clock(value).into());
                    ui.set_lyric_follow(true);
                }
                None => {}
            }

            let current_token = generation.load(Ordering::Relaxed);
            let snapshot = audio
                .snapshot
                .lock()
                .ok()
                .map(|state| state.clone())
                .unwrap_or_default();
            // A snapshot from a previous generation must never drive the new
            // track's UI. `u64::MAX` is the permanent audio-output error.
            if snapshot.token != current_token && snapshot.token != u64::MAX {
                return;
            }
            set_media_playing(snapshot.playing);
            if ui.get_window_minimized() {
                // Keep media keys and auto-advance working while the window is
                // minimized, but skip the UI/label/lyrics writes that would
                // fault pages back into the working set.
                if snapshot.loaded || snapshot.ended || snapshot.error.is_some() {
                    // Clear the stale loading flag so SMTC play/pause keeps
                    // working after a next/previous while minimized.
                    if ui.get_loading() {
                        ui.set_loading(false);
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
                return;
            }
            if snapshot.duration > 0.5 {
                let current = ui.get_duration();
                if (current - snapshot.duration).abs() > 0.5 {
                    ui.set_duration(snapshot.duration);
                    ui.set_duration_text(format_clock(snapshot.duration).into());
                }
            }
            if let Some(error) = snapshot.error.clone() {
                if last_audio_error.as_deref() != Some(error.as_str()) {
                    last_audio_error = Some(error.clone());
                    ui.set_status_text(error.into());
                }
                ui.set_loading(false);
                ui.set_playing(false);
                return;
            }
            last_audio_error = None;
            if snapshot.loaded || snapshot.ended {
                let duration = ui.get_duration().max(0.01);
                ui.set_progress(snapshot.position.min(duration));
                ui.set_position_text(format_clock(snapshot.position).into());
            }
            if snapshot.loaded {
                if ui.get_status_text().as_str() == "正在加载音源…" {
                    ui.set_status_text("".into());
                }
                ui.set_loading(false);
            } else if snapshot.ended {
                ui.set_loading(false);
            }
            ui.set_playing(snapshot.playing);
            if snapshot.loaded || snapshot.ended {
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
                    // Public recommendations are available without login.
                    load_home(&task_runtime, state.clone(), weak.clone(), catalog.clone());
                }
                Err(error) => {
                    update_ui(&weak, move |ui| {
                        ui.set_login_open(true);
                        ui.set_status_text(error.into());
                        ui.invoke_refresh_qr();
                    });
                    load_home(&task_runtime, state.clone(), weak.clone(), catalog.clone());
                }
            }
        });
    }

    let result = ui.run();
    let position = current_position(&audio, &player, &generation);
    save_player(&player, position);
    SHUTDOWN_SAVED.store(true, Ordering::Relaxed);
    if let Err(error) = result {
        logger::log(
            "error",
            "app",
            &format!("UI event loop ended with error: {error}"),
        );
    }
    // The audio, SMTC and window watcher threads are intentionally detached.
    // Exit explicitly so they can never keep the portable exe locked.
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::{detail_share_url, playlist_id_from_input};

    #[test]
    fn playlist_id_accepts_plain_numbers() {
        assert_eq!(playlist_id_from_input("123456").as_deref(), Some("123456"));
    }

    #[test]
    fn playlist_id_accepts_links() {
        assert_eq!(
            playlist_id_from_input("https://music.163.com/#/playlist?id=98765&userid=1").as_deref(),
            Some("98765")
        );
        assert_eq!(
            playlist_id_from_input("https://music.163.com/playlist/45678/").as_deref(),
            Some("45678")
        );
    }

    #[test]
    fn playlist_id_rejects_invalid_text() {
        assert_eq!(playlist_id_from_input("not a playlist"), None);
        assert_eq!(playlist_id_from_input(""), None);
    }

    #[test]
    fn detail_links_are_restricted_to_numeric_ids() {
        assert_eq!(
            detail_share_url("playlist", "123").as_deref(),
            Some("https://music.163.com/#/playlist?id=123")
        );
        assert_eq!(
            detail_share_url("artist", "456").as_deref(),
            Some("https://music.163.com/#/artist?id=456")
        );
        assert_eq!(
            detail_share_url("album", "789").as_deref(),
            Some("https://music.163.com/#/album?id=789")
        );
        assert_eq!(detail_share_url("playlist", ""), None);
        assert_eq!(detail_share_url("playlist", "../evil"), None);
        assert_eq!(
            detail_share_url("daily", "daily").as_deref(),
            Some("https://music.163.com/#/discover/recommend")
        );
    }
}
