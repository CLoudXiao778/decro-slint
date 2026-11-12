//! Windows system media transport controls (SMTC).
//!
//! This is the Windows media-key / taskbar flyout integration. The WinRT
//! objects live on their own thread; the UI only sends metadata updates and
//! polls an atomic for play/pause/next/previous button presses.

use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::time::Duration;

const CMD_TOGGLE: u8 = 1;
const CMD_NEXT: u8 = 2;
const CMD_PREVIOUS: u8 = 3;
const NO_SEEK: u32 = u32::MAX;

static COMMAND: AtomicU8 = AtomicU8::new(0);
static SEEK_MS: AtomicU32 = AtomicU32::new(NO_SEEK);

#[derive(Clone, Copy, Debug)]
pub enum MediaCommand {
    Toggle,
    Next,
    Previous,
    Seek(f32),
}

pub fn start() {
    imp::start();
}

pub fn update_track(title: &str, artist: &str, album: &str, cover: Option<&Path>) {
    imp::update_track(title, artist, album, cover);
}

pub fn update_playback(playing: bool) {
    imp::update_playback(playing);
}

pub fn update_position(position: f32, duration: f32) {
    imp::update_position(position, duration);
}

pub fn clear() {
    imp::clear();
}

pub fn copy_text(text: &str) -> bool {
    imp::copy_text(text)
}

pub fn take_command() -> Option<MediaCommand> {
    let seek = SEEK_MS.swap(NO_SEEK, Ordering::Relaxed);
    if seek != NO_SEEK {
        return Some(MediaCommand::Seek(seek as f32 / 1000.0));
    }
    match COMMAND.swap(0, Ordering::Relaxed) {
        CMD_TOGGLE => Some(MediaCommand::Toggle),
        CMD_NEXT => Some(MediaCommand::Next),
        CMD_PREVIOUS => Some(MediaCommand::Previous),
        _ => None,
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn start() {}
    pub fn update_track(_: &str, _: &str, _: &str, _: Option<&Path>) {}
    pub fn update_playback(_: bool) {}
    pub fn update_position(_: f32, _: f32) {}
    pub fn clear() {}
    pub fn copy_text(_: &str) -> bool {
        false
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Mutex, OnceLock};

    use windows::core::Interface;
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Foundation::TimeSpan;
    use windows::Foundation::TypedEventHandler;
    use windows::Media::{
        MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
        SystemMediaTransportControls, SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsDisplayUpdater, SystemMediaTransportControlsTimelineProperties,
    };
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::{IRandomAccessStream, RandomAccessStreamReference};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HBITMAP,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::WinRT::{
        ISystemMediaTransportControlsInterop, RoGetActivationFactory, RoInitialize,
        RO_INIT_MULTITHREADED,
    };
    use windows::Win32::UI::Shell::{
        DefSubclassProc, ITaskbarList3, SetWindowSubclass, TaskbarList, TBPF_NOPROGRESS,
        TBPF_NORMAL, TBPF_PAUSED, THBF_ENABLED, THBN_CLICKED, THB_FLAGS, THB_ICON, THB_TOOLTIP,
        THUMBBUTTON, THUMBBUTTONMASK,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, DestroyIcon, FindWindowW, HICON, ICONINFO, WM_COMMAND,
    };

    enum MediaUpdate {
        Track {
            title: String,
            artist: String,
            album: String,
            cover: Option<PathBuf>,
        },
        Playback(bool),
        Position {
            position: f32,
            duration: f32,
        },
        Clear,
    }

    enum TaskbarUpdate {
        Track,
        Playback(bool),
        Position { position: f32, duration: f32 },
        Clear,
    }

    static TX: OnceLock<Mutex<Sender<MediaUpdate>>> = OnceLock::new();
    static TASKBAR_TX: OnceLock<Mutex<Sender<TaskbarUpdate>>> = OnceLock::new();
    static STARTED: OnceLock<()> = OnceLock::new();

    pub fn start() {
        if STARTED.set(()).is_err() {
            return;
        }
        let (tx, rx) = mpsc::channel::<MediaUpdate>();
        let _ = TX.set(Mutex::new(tx));
        let _ = std::thread::Builder::new()
            .name("decro-smtc".into())
            .spawn(move || worker(rx));
        let (taskbar_tx, taskbar_rx) = mpsc::channel::<TaskbarUpdate>();
        let _ = TASKBAR_TX.set(Mutex::new(taskbar_tx));
        let _ = std::thread::Builder::new()
            .name("decro-taskbar".into())
            .spawn(move || taskbar_worker(taskbar_rx));
    }

    fn send_taskbar(update: TaskbarUpdate) {
        if let Some(tx) = TASKBAR_TX.get() {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(update);
            }
        }
    }

    pub fn update_track(title: &str, artist: &str, album: &str, cover: Option<&Path>) {
        if let Some(tx) = TX.get() {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(MediaUpdate::Track {
                    title: title.to_string(),
                    artist: artist.to_string(),
                    album: album.to_string(),
                    cover: cover.map(Path::to_path_buf),
                });
            }
        }
        send_taskbar(TaskbarUpdate::Track);
    }

    pub fn update_playback(playing: bool) {
        if let Some(tx) = TX.get() {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(MediaUpdate::Playback(playing));
            }
        }
        send_taskbar(TaskbarUpdate::Playback(playing));
    }

    pub fn update_position(position: f32, duration: f32) {
        if let Some(tx) = TX.get() {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(MediaUpdate::Position { position, duration });
            }
        }
        send_taskbar(TaskbarUpdate::Position { position, duration });
    }

    pub fn clear() {
        if let Some(tx) = TX.get() {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(MediaUpdate::Clear);
            }
        }
        send_taskbar(TaskbarUpdate::Clear);
    }

    fn find_window() -> Option<HWND> {
        for _ in 0..300 {
            if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR::null(), w!("Decro")) } {
                return Some(hwnd);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }

    struct Taskbar {
        list: ITaskbarList3,
        hwnd: HWND,
        play_icon: HICON,
        pause_icon: HICON,
        playing: bool,
        has_track: bool,
    }

    impl Taskbar {
        fn new(hwnd: HWND) -> Option<Self> {
            let list: ITaskbarList3 =
                unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_ALL).ok()? };
            unsafe {
                let _ = list.HrInit();
            }
            let prev_icon = make_taskbar_icon(TaskbarIcon::Previous)?;
            let next_icon = make_taskbar_icon(TaskbarIcon::Next)?;
            let play_icon = make_taskbar_icon(TaskbarIcon::Play)?;
            let pause_icon = make_taskbar_icon(TaskbarIcon::Pause)?;
            let mut buttons = [
                taskbar_button(1, prev_icon, "上一首"),
                taskbar_button(2, play_icon, "播放 / 暂停"),
                taskbar_button(3, next_icon, "下一首"),
            ];
            // The taskbar button may not exist during the first frames after
            // window creation; retry briefly before giving up.
            let mut added = false;
            for _ in 0..50 {
                if unsafe { list.ThumbBarAddButtons(hwnd, &mut buttons) }.is_ok() {
                    added = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if !added {
                crate::logger::log("warn", "media", "taskbar thumb buttons were not added");
            }
            unsafe {
                let _ = SetWindowSubclass(hwnd, Some(taskbar_subclass_proc), 0xDEC0, 0);
            }
            Some(Self {
                list,
                hwnd,
                play_icon,
                pause_icon,
                playing: false,
                has_track: false,
            })
        }

        fn set_playback(&mut self, playing: bool) {
            self.playing = playing;
            if !self.has_track {
                return;
            }
            let icon = if playing {
                self.pause_icon
            } else {
                self.play_icon
            };
            let mut buttons = [THUMBBUTTON {
                dwMask: THB_ICON,
                iId: 2,
                hIcon: icon,
                ..Default::default()
            }];
            let _ = unsafe { self.list.ThumbBarUpdateButtons(self.hwnd, &mut buttons) };
            let state = if playing { TBPF_NORMAL } else { TBPF_PAUSED };
            let _ = unsafe { self.list.SetProgressState(self.hwnd, state) };
        }

        fn set_track(&mut self) {
            self.has_track = true;
            self.set_buttons_enabled(true);
            let _ = unsafe { self.list.SetProgressValue(self.hwnd, 0, 1000) };
            self.set_playback(self.playing);
        }

        fn set_buttons_enabled(&self, enabled: bool) {
            let flags = if enabled {
                THBF_ENABLED
            } else {
                windows::Win32::UI::Shell::THBF_DISABLED
            };
            let mut buttons = [
                THUMBBUTTON {
                    dwMask: THB_FLAGS,
                    iId: 1,
                    dwFlags: flags,
                    ..Default::default()
                },
                THUMBBUTTON {
                    dwMask: THB_FLAGS,
                    iId: 2,
                    dwFlags: flags,
                    ..Default::default()
                },
                THUMBBUTTON {
                    dwMask: THB_FLAGS,
                    iId: 3,
                    dwFlags: flags,
                    ..Default::default()
                },
            ];
            let _ = unsafe { self.list.ThumbBarUpdateButtons(self.hwnd, &mut buttons) };
        }

        fn set_position(&mut self, position: f32, duration: f32) {
            if !self.has_track || duration <= 0.5 {
                return;
            }
            let total = (duration.max(0.0) * 1000.0) as u64;
            let value = (position.clamp(0.0, duration) * 1000.0) as u64;
            let _ = unsafe { self.list.SetProgressValue(self.hwnd, value, total) };
        }

        fn clear(&mut self) {
            self.has_track = false;
            self.set_buttons_enabled(false);
            let _ = unsafe { self.list.SetProgressState(self.hwnd, TBPF_NOPROGRESS) };
        }
    }

    impl Drop for Taskbar {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyIcon(self.play_icon);
                let _ = DestroyIcon(self.pause_icon);
            }
        }
    }

    #[derive(Clone, Copy)]
    enum TaskbarIcon {
        Play,
        Pause,
        Previous,
        Next,
    }

    fn taskbar_button(id: u32, icon: HICON, tip: &str) -> THUMBBUTTON {
        let mut sz_tip = [0u16; 260];
        for (slot, unit) in sz_tip.iter_mut().zip(tip.encode_utf16()) {
            *slot = unit;
        }
        THUMBBUTTON {
            dwMask: THUMBBUTTONMASK(THB_ICON.0 | THB_TOOLTIP.0 | THB_FLAGS.0),
            iId: id,
            hIcon: icon,
            szTip: sz_tip,
            dwFlags: THBF_ENABLED,
            ..Default::default()
        }
    }

    fn make_taskbar_icon(kind: TaskbarIcon) -> Option<HICON> {
        const SIZE: usize = 16;
        let mut pixels = vec![0u8; SIZE * SIZE * 4];
        let mut set = |x: usize, y: usize| {
            if x < SIZE && y < SIZE {
                let index = (y * SIZE + x) * 4;
                pixels[index] = 255;
                pixels[index + 1] = 255;
                pixels[index + 2] = 255;
                pixels[index + 3] = 255;
            }
        };
        match kind {
            TaskbarIcon::Play => {
                for y in 3..13usize {
                    let width = 8usize.saturating_sub(y.abs_diff(7));
                    for x in 4..(4 + width).min(14) {
                        set(x, y);
                    }
                }
            }
            TaskbarIcon::Pause => {
                for y in 3..13usize {
                    for x in 4..7usize {
                        set(x, y);
                    }
                    for x in 9..12usize {
                        set(x, y);
                    }
                }
            }
            TaskbarIcon::Previous => {
                for y in 3..13usize {
                    for x in 3..6usize {
                        set(x, y);
                    }
                    let width = 7usize.saturating_sub(y.abs_diff(7));
                    for x in (13 - width)..13usize {
                        set(x, y);
                    }
                }
            }
            TaskbarIcon::Next => {
                for y in 3..13usize {
                    let width = 7usize.saturating_sub(y.abs_diff(7));
                    for x in 4..(4 + width).min(10) {
                        set(x, y);
                    }
                    for x in 11..14usize {
                        set(x, y);
                    }
                }
            }
        }
        unsafe {
            let mut info = BITMAPINFO::default();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: SIZE as i32,
                biHeight: -(SIZE as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let color: HBITMAP =
                CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            if bits.is_null() {
                let _ = DeleteObject(color.into());
                return None;
            }
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
            let mut mask_bits = vec![0u8; SIZE * 2];
            for y in 0..SIZE {
                for x in 0..SIZE {
                    if pixels[(y * SIZE + x) * 4 + 3] == 0 {
                        mask_bits[y * 2 + x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            let mask = CreateBitmap(
                SIZE as i32,
                SIZE as i32,
                1,
                1,
                Some(mask_bits.as_ptr() as *const std::ffi::c_void),
            );
            let icon_info = ICONINFO {
                fIcon: true.into(),
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: color,
            };
            let icon = CreateIconIndirect(&icon_info).ok();
            let _ = DeleteObject(color.into());
            let _ = DeleteObject(mask.into());
            icon
        }
    }

    unsafe extern "system" fn taskbar_subclass_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if message == WM_COMMAND {
            let id = (wparam.0 & 0xffff) as u32;
            let code = ((wparam.0 >> 16) & 0xffff) as u32;
            if code == THBN_CLICKED {
                let command = match id {
                    1 => Some(CMD_PREVIOUS),
                    2 => Some(CMD_TOGGLE),
                    3 => Some(CMD_NEXT),
                    _ => None,
                };
                if let Some(command) = command {
                    COMMAND.store(command, Ordering::Relaxed);
                }
            }
        }
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    fn taskbar_worker(rx: mpsc::Receiver<TaskbarUpdate>) {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let Some(hwnd) = find_window() else {
            crate::logger::log("warn", "media", "taskbar window not found");
            return;
        };
        let mut taskbar = Taskbar::new(hwnd);
        if taskbar.is_none() {
            crate::logger::log("warn", "media", "taskbar buttons unavailable");
        }
        while let Ok(update) = rx.recv() {
            let Some(taskbar) = &mut taskbar else {
                continue;
            };
            match update {
                TaskbarUpdate::Track => taskbar.set_track(),
                TaskbarUpdate::Playback(playing) => taskbar.set_playback(playing),
                TaskbarUpdate::Position { position, duration } => {
                    taskbar.set_position(position, duration)
                }
                TaskbarUpdate::Clear => taskbar.clear(),
            }
        }
    }

    fn worker(rx: mpsc::Receiver<MediaUpdate>) {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let Some(hwnd) = find_window() else {
            crate::logger::log("warn", "media", "SMTC window not found");
            return;
        };
        let factory: ISystemMediaTransportControlsInterop = match unsafe {
            RoGetActivationFactory(&HSTRING::from("Windows.Media.SystemMediaTransportControls"))
        } {
            Ok(factory) => factory,
            Err(error) => {
                crate::logger::log("warn", "media", &format!("SMTC factory failed: {error}"));
                return;
            }
        };
        let smtc: SystemMediaTransportControls = match unsafe { factory.GetForWindow(hwnd) } {
            Ok(smtc) => smtc,
            Err(error) => {
                crate::logger::log("warn", "media", &format!("SMTC init failed: {error}"));
                return;
            }
        };

        let _ = smtc.SetIsEnabled(true);
        let _ = smtc.SetIsPlayEnabled(true);
        let _ = smtc.SetIsPauseEnabled(true);
        let _ = smtc.SetIsNextEnabled(true);
        let _ = smtc.SetIsPreviousEnabled(true);
        let _ = smtc.SetPlaybackStatus(MediaPlaybackStatus::Paused);

        let handler = TypedEventHandler::<
            SystemMediaTransportControls,
            SystemMediaTransportControlsButtonPressedEventArgs,
        >::new(|_sender, args| {
            if let Ok(args) = args.ok() {
                if let Ok(button) = args.Button() {
                    let command = match button.0 {
                        0 | 1 => Some(CMD_TOGGLE), // Play / Pause
                        6 => Some(CMD_NEXT),
                        7 => Some(CMD_PREVIOUS),
                        _ => None,
                    };
                    if let Some(command) = command {
                        COMMAND.store(command, Ordering::Relaxed);
                    }
                }
            }
            Ok(())
        });
        if let Err(error) = smtc.ButtonPressed(&handler) {
            crate::logger::log(
                "warn",
                "media",
                &format!("SMTC button handler failed: {error}"),
            );
        }

        let seek_handler = TypedEventHandler::<
            SystemMediaTransportControls,
            PlaybackPositionChangeRequestedEventArgs,
        >::new(|_sender, args| {
            if let Ok(args) = args.ok() {
                if let Ok(position) = args.RequestedPlaybackPosition() {
                    let millis = (position.Duration / 10_000).max(0).min(u32::MAX as i64);
                    SEEK_MS.store(millis as u32, Ordering::Relaxed);
                }
            }
            Ok(())
        });
        if let Err(error) = smtc.PlaybackPositionChangeRequested(&seek_handler) {
            crate::logger::log(
                "warn",
                "media",
                &format!("SMTC seek handler failed: {error}"),
            );
        }

        let updater = smtc.DisplayUpdater().ok();
        if let Some(updater) = &updater {
            let _ = updater.SetType(MediaPlaybackType::Music);
        }

        while let Ok(update) = rx.recv() {
            match update {
                MediaUpdate::Track {
                    title,
                    artist,
                    album,
                    cover,
                } => {
                    if let Some(updater) = &updater {
                        if cover.is_none() {
                            // Do not leave the previous track's artwork on the
                            // system flyout when the new track has no cover.
                            let _ = updater.ClearAll();
                            let _ = updater.SetType(MediaPlaybackType::Music);
                        }
                        if let Ok(properties) = updater.MusicProperties() {
                            let _ = properties.SetTitle(&HSTRING::from(title.as_str()));
                            let _ = properties.SetArtist(&HSTRING::from(artist.as_str()));
                            let _ = properties.SetAlbumTitle(&HSTRING::from(album.as_str()));
                        }
                        if let Some(path) = cover {
                            apply_thumbnail(updater, &path);
                        }
                        let _ = updater.Update();
                    }
                }
                MediaUpdate::Playback(playing) => {
                    let status = if playing {
                        MediaPlaybackStatus::Playing
                    } else {
                        MediaPlaybackStatus::Paused
                    };
                    let _ = smtc.SetPlaybackStatus(status);
                }
                MediaUpdate::Position { position, duration } => {
                    if duration > 0.0 {
                        if let Ok(timeline) = SystemMediaTransportControlsTimelineProperties::new()
                        {
                            let to_ticks = |seconds: f32| TimeSpan {
                                Duration: (seconds.max(0.0) as f64 * 10_000_000.0) as i64,
                            };
                            let _ = timeline.SetStartTime(to_ticks(0.0));
                            let _ = timeline.SetEndTime(to_ticks(duration));
                            let _ = timeline.SetMinSeekTime(to_ticks(0.0));
                            let _ = timeline.SetMaxSeekTime(to_ticks(duration));
                            let _ = timeline.SetPosition(to_ticks(position.min(duration)));
                            let _ = smtc.UpdateTimelineProperties(&timeline);
                        }
                    }
                }
                MediaUpdate::Clear => {
                    if let Some(updater) = &updater {
                        let _ = updater.ClearAll();
                    }
                    let _ = smtc.SetPlaybackStatus(MediaPlaybackStatus::Closed);
                }
            }
        }
    }

    pub fn copy_text(text: &str) -> bool {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
        };
        use windows::Win32::System::Memory::{
            GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
        };
        use windows::Win32::System::Ole::CF_UNICODETEXT;

        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = wide.len() * std::mem::size_of::<u16>();
        let result = (|| -> windows::core::Result<()> {
            unsafe {
                let handle = GlobalAlloc(GMEM_MOVEABLE, bytes)?;
                let target = GlobalLock(handle) as *mut u16;
                if target.is_null() {
                    return Err(windows::core::Error::from_hresult(
                        windows::Win32::Foundation::E_OUTOFMEMORY,
                    ));
                }
                std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
                let _ = GlobalUnlock(handle);
                OpenClipboard(None)?;
                EmptyClipboard()?;
                let set = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(handle.0)));
                let _ = CloseClipboard();
                set?;
            }
            Ok(())
        })();
        result.is_ok()
    }

    fn apply_thumbnail(updater: &SystemMediaTransportControlsDisplayUpdater, path: &Path) {
        let path = HSTRING::from(path.to_string_lossy().as_ref());
        let Ok(operation) = StorageFile::GetFileFromPathAsync(&path) else {
            return;
        };
        let Ok(file) = operation.join() else {
            return;
        };
        let Ok(stream) = file.OpenReadAsync().and_then(|operation| operation.join()) else {
            return;
        };
        let Ok(stream) = stream.cast::<IRandomAccessStream>() else {
            return;
        };
        let Ok(reference) = RandomAccessStreamReference::CreateFromStream(&stream) else {
            return;
        };
        let _ = updater.SetThumbnail(&reference);
    }
}
