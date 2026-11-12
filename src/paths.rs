use std::fs;
use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("data")))
        .unwrap_or_else(|| std::env::temp_dir().join("decro-slint-data"));
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn auth_path() -> PathBuf {
    data_dir().join("netease-auth.bin")
}

pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn cover_dir() -> PathBuf {
    let dir = data_dir().join("covers");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Keep the cover cache from growing without bound. Offline/streaming use can
/// leave thousands of files; only the newest N are retained.
pub fn trim_cover_cache(max_files: usize) {
    let dir = cover_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            Some((modified, path))
        })
        .collect();
    if files.len() <= max_files {
        return;
    }
    files.sort_by_key(|(modified, _)| *modified);
    let remove_count = files.len() - max_files;
    for (_, path) in files.into_iter().take(remove_count) {
        let _ = fs::remove_file(path);
    }
}

/// Temporary, per-play audio files. Streaming the source to disk keeps the
/// memory footprint independent of the track length (important for Hi-Res).
pub fn audio_cache_dir() -> PathBuf {
    let dir = data_dir().join("audio-cache");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Remove leftovers from a previous run. Called once at startup before any
/// playback begins; files of the current session are deleted track-by-track.
pub fn clean_audio_cache() {
    let dir = audio_cache_dir();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let _ = fs::remove_file(entry.path());
        }
    }
}
