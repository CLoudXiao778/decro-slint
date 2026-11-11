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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use std::rc::Rc;

use slint::{Color, ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel, Weak};
use state::{AppState, ProxyInfo};

slint::include_modules!();

#[derive(Default, Clone)]
struct Catalog {
    recent: Vec<netease::TrackDto>,
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
    ended: bool,
    error: Option<String>,
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
            if let Ok(mut state) = snap.lock() {
                state.position = position;
                state.playing = playing;
                if ended {
                    state.ended = true;
                }
            }
            if ended {
                loaded = false;
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

fn playlist_item(playlist: &netease::PlaylistDto) -> PlaylistItem {
    PlaylistItem {
        id: playlist.id.clone().into(),
        title: playlist.name.clone().into(),
        meta: format!("{} · {} 首", playlist.creator, playlist.track_count).into(),
        cover: color_for(&playlist.id),
    }
}

fn track_items(items: &[netease::TrackDto]) -> Vec<TrackItem> {
    items.iter().map(track_item).collect()
}

fn playlist_items(items: &[netease::PlaylistDto]) -> Vec<PlaylistItem> {
    items.iter().map(playlist_item).collect()
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

fn load_volume() -> f32 {
    fs::read_to_string(paths::data_dir().join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.get("volume").and_then(|v| v.as_f64()))
        .map(|value| (value as f32).clamp(0.0, 1.0))
        .unwrap_or(0.7)
}

fn save_volume(value: f32) {
    let path = paths::data_dir().join("settings.json");
    let payload = format!("{{\"volume\":{:.4}}}", value.clamp(0.0, 1.0));
    let _ = fs::write(path, payload);
}

fn find_track(catalog: &Catalog, id: &str) -> Option<(netease::TrackDto, Vec<netease::TrackDto>)> {
    for list in [
        &catalog.detail,
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
    track: netease::TrackDto,
) {
    let request = generation.fetch_add(1, Ordering::Relaxed) + 1;
    let duration = track.duration_ms as f32 / 1000.0;
    let title = track.name.clone();
    let artist = track.artist.clone();
    let quality = track.quality.clone();
    let cover = color_for(&track.id);
    update_ui(&ui, move |ui| {
        ui.set_current_title(title.into());
        ui.set_current_artist(artist.into());
        ui.set_current_quality(quality.into());
        ui.set_current_cover(cover);
        ui.set_duration(duration);
        ui.set_progress(0.0);
        ui.set_playing(true);
        ui.set_status_text("正在加载音源…".into());
    });
    runtime.spawn(async move {
        let url = match netease::resolve_song_url(&state, &track.id, None).await {
            Ok(url) => url,
            Err(error) => {
                update_ui(&ui, move |ui| ui.set_status_text(error.into()));
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
                update_ui(&ui, move |ui| ui.set_status_text(message.into()));
                return;
            }
        };
        if !response.url().host_str().is_some_and(state::is_allowed_audio_host) {
            update_ui(&ui, move |ui| ui.set_status_text("音源域名异常".into()));
            return;
        }
        if !response.status().is_success() {
            let message = format!("音源返回 HTTP {}", response.status());
            update_ui(&ui, move |ui| ui.set_status_text(message.into()));
            return;
        }
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes.to_vec(),
            Err(error) => {
                let message = format!("读取音源失败: {error}");
                update_ui(&ui, move |ui| ui.set_status_text(message.into()));
                return;
            }
        };
        if generation.load(Ordering::Relaxed) != request {
            return;
        }
        let _ = audio.tx.send(AudioCmd::Load(bytes, duration));
        let id = track.id.clone();
        if let Ok(data) = netease::lyric(&state, id).await {
            let lines = data.lines.clone();
            if generation.load(Ordering::Relaxed) != request {
                return;
            }
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
    start_track(
        runtime,
        state.clone(),
        ui.clone(),
        audio.clone(),
        player.clone(),
        lyrics.clone(),
        generation.clone(),
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
                    catalog.new_songs = home.new_songs.clone();
                    catalog.playlists = home.recommended.clone();
                }
                let recent = track_items(&home.recent);
                let new_songs = track_items(&home.new_songs);
                let playlists = playlist_items(&home.recommended);
                update_ui(&ui, move |ui| {
                    ui.set_recent_tracks(ModelRc::from(Rc::new(VecModel::from(recent))));
                    ui.set_new_tracks(ModelRc::from(Rc::new(VecModel::from(new_songs))));
                    ui.set_playlists(ModelRc::from(Rc::new(VecModel::from(playlists))));
                    ui.set_status_text(format!("已加载 {} 个推荐歌单", home.recommended.len()).into());
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
                let playlists = playlist_items(&library.playlists);
                let recent = track_items(&library.recent);
                update_ui(&ui, move |ui| {
                    ui.set_library_playlists(ModelRc::from(Rc::new(VecModel::from(playlists))));
                    ui.set_library_tracks(ModelRc::from(Rc::new(VecModel::from(recent))));
                    ui.set_status_text(format!("资料库：{} 个歌单", library.playlists.len()).into());
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
    let audio = spawn_audio();
    let volume = load_volume();
    ui.set_volume(volume);
    let _ = audio.tx.send(AudioCmd::Volume(volume));

    ui.set_status_text(paths::data_dir().display().to_string().into());
    ui.set_login_open(true);

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let catalog = catalog.clone();
        ui.on_refresh_qr(move || {
            let runtime = runtime.clone();
            let task_runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
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
                            match auth::qr_login_poll(&state, key.clone()).await {
                                Ok(poll) => {
                                    if let Some(account) = poll.account.clone() {
                                        update_ui(&weak, move |ui| {
                                            ui.set_logged_in(true);
                                            ui.set_login_open(false);
                                            ui.set_account_name(account.nickname.into());
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
                    ui.set_status_text("已退出登录".into());
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
        ui.on_search(move |keyword| {
            let runtime = runtime.clone();
            let state = state.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            let keyword = keyword.to_string();
            runtime.spawn(async move {
                if let Some(ui) = weak.upgrade() {
                    ui.set_page("search".into());
                    ui.set_status_text("正在搜索…".into());
                }
                match netease::search_music(&state, keyword).await {
                    Ok(data) => {
                        if let Ok(mut catalog) = catalog.lock() {
                            catalog.search_songs = data.songs.clone();
                            catalog.search_playlists = data.playlists.clone();
                        }
                        let songs = track_items(&data.songs);
                        let playlists = playlist_items(&data.playlists);
                        update_ui(&weak, move |ui| {
                            ui.set_search_tracks(ModelRc::from(Rc::new(VecModel::from(songs))));
                            ui.set_search_playlists(ModelRc::from(Rc::new(VecModel::from(playlists))));
                            ui.set_status_text(format!("搜索到 {} 首歌曲", data.songs.len()).into());
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
                        update_ui(&weak, move |ui| {
                            ui.set_page("detail".into());
                            ui.set_detail_title(title.into());
                            ui.set_detail_meta(meta.into());
                            ui.set_detail_tracks(ModelRc::from(Rc::new(VecModel::from(tracks))));
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
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        let audio = audio.clone();
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
            start_track(&runtime, state.clone(), weak.clone(), audio.clone(), player.clone(), lyrics.clone(), generation.clone(), track);
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
            start_track(&runtime, state.clone(), weak.clone(), audio.clone(), player.clone(), lyrics.clone(), generation.clone(), track);
        });
    }

    {
        let audio = audio.clone();
        ui.on_toggle_play(move || {
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
        ui.on_volume_changed(move |value| {
            let _ = audio.tx.send(AudioCmd::Volume(value));
        });
    }

    {
        ui.on_volume_released(move |value| save_volume(value));
    }

    {
        let runtime = runtime.clone();
        let state = state.clone();
        let weak = weak.clone();
        let audio = audio.clone();
        let player = player.clone();
        let lyrics = lyrics.clone();
        let generation = generation.clone();
        ui.on_next(move || {
            play_relative(1, &runtime, &state, &weak, &audio, &player, &lyrics, &generation);
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
        ui.on_prev(move || {
            play_relative(-1, &runtime, &state, &weak, &audio, &player, &lyrics, &generation);
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
                play_relative(1, &runtime, &state, &weak, &audio, &player, &lyrics, &generation);
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
                    });
                    load_home(&task_runtime, state.clone(), weak.clone(), catalog.clone());
                    load_library(&task_runtime, state.clone(), weak.clone(), catalog);
                }
                Ok(None) => {
                    update_ui(&weak, move |ui| {
                        ui.set_login_open(true);
                        ui.set_status_text("请扫码登录，或导入网页版 Cookie".into());
                    });
                }
                Err(error) => update_ui(&weak, move |ui| ui.set_status_text(error.into())),
            }
        });
    }

    ui.run()?;
    Ok(())
}
