//! One-day cache for the home page.
//!
//! NetEase's daily recommendations do not change within a day, so fetching
//! them on every visit only adds latency and can make overlapping responses
//! overwrite each other. The cache is keyed by local date and account id.
//! Recent plays are intentionally not cached; they are merged from the local
//! play history on every home render.

use std::fs;
#[cfg(not(windows))]
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::netease::{HomeFeed, PlaylistDto, TrackDto};

const CACHE_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedHome {
    pub version: u32,
    pub date: String,
    pub account_id: String,
    pub recommended: Vec<PlaylistDto>,
    #[serde(default)]
    pub feature: Option<PlaylistDto>,
    pub daily: Vec<TrackDto>,
    pub new_songs: Vec<TrackDto>,
}

impl CachedHome {
    pub fn from_home(account_id: &str, home: &HomeFeed) -> Self {
        Self {
            version: CACHE_VERSION,
            date: today_key(),
            account_id: account_id.to_string(),
            recommended: home.recommended.clone(),
            feature: home.feature.clone(),
            daily: home.daily.clone(),
            new_songs: home.new_songs.clone(),
        }
    }

    pub fn matches(&self, account_id: &str) -> bool {
        self.version == CACHE_VERSION && self.date == today_key() && self.account_id == account_id
    }

    pub fn to_home_feed(&self) -> HomeFeed {
        HomeFeed {
            recommended: self.recommended.clone(),
            feature: self.feature.clone(),
            new_songs: self.new_songs.clone(),
            daily: self.daily.clone(),
            recent: Vec::new(),
            error: None,
        }
    }
}

fn cache_path() -> std::path::PathBuf {
    crate::paths::data_dir().join("home-cache.json")
}

pub fn load() -> Option<CachedHome> {
    let text = fs::read_to_string(cache_path()).ok()?;
    let cache: CachedHome = serde_json::from_str(&text).ok()?;
    if cache.version == CACHE_VERSION {
        Some(cache)
    } else {
        None
    }
}

pub fn save(cache: &CachedHome) {
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = fs::write(cache_path(), text);
    }
}

pub fn clear() {
    let _ = fs::remove_file(cache_path());
}

/// Cache day used by the home page. NetEase refreshes the daily
/// recommendations at 06:00 local time, so a launch at 05:00 still belongs to
/// the previous recommendation day.
pub fn today_key() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;

        let mut time: SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe {
            GetLocalTime(&mut time);
        }
        let mut year = time.wYear as i32;
        let mut month = time.wMonth as u32;
        let mut day = time.wDay as u32;
        if time.wHour < 6 {
            if day > 1 {
                day -= 1;
            } else if month > 1 {
                month -= 1;
                day = days_in_month(year, month);
            } else {
                year -= 1;
                month = 12;
                day = 31;
            }
        }
        format!("{year:04}-{month:02}-{day:02}")
    }
    #[cfg(not(windows))]
    {
        let days = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs().saturating_sub(6 * 3_600) / 86_400)
            .unwrap_or(0);
        format!("utc-day-{days}")
    }
}

#[cfg(windows)]
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}
