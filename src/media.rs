//! Windows system media transport controls (SMTC).
//!
//! This is the Windows media-key / taskbar flyout integration. The WinRT
//! objects live on their own thread; the UI only sends metadata updates and
//! polls an atomic for play/pause/next/previous button presses.

use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

const CMD_TOGGLE: u8 = 1;
const CMD_NEXT: u8 = 2;
const CMD_PREVIOUS: u8 = 3;

static COMMAND: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Debug)]
pub enum MediaCommand {
    Toggle,
    Next,
    Previous,
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

pub fn take_command() -> Option<MediaCommand> {
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
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Mutex, OnceLock};

    use windows::core::Interface;
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Foundation::TypedEventHandler;
    use windows::Media::{
        MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
        SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsDisplayUpdater,
    };
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::{IRandomAccessStream, RandomAccessStreamReference};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::{
        ISystemMediaTransportControlsInterop, RoGetActivationFactory, RoInitialize,
        RO_INIT_MULTITHREADED,
    };
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

    enum MediaUpdate {
        Track {
            title: String,
            artist: String,
            album: String,
            cover: Option<PathBuf>,
        },
        Playback(bool),
    }

    static TX: OnceLock<Mutex<Sender<MediaUpdate>>> = OnceLock::new();
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
    }

    pub fn update_playback(playing: bool) {
        if let Some(tx) = TX.get() {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(MediaUpdate::Playback(playing));
            }
        }
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
            }
        }
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
