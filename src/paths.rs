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
