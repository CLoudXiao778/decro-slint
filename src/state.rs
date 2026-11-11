use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const API_BASE: &str = "https://music.163.com";
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub user_id: String,
    pub nickname: String,
    pub avatar_url: String,
}

#[derive(Clone, Debug)]
pub struct ProxyInfo {
    pub base: String,
    pub token: String,
}

/// Shared state for every Tauri command and the local audio proxy.
///
/// The type is cheap to clone: all mutable state sits behind `Arc<Mutex<_>>`,
/// which lets the axum server keep its own handle without knowing about Tauri.
pub fn is_allowed_audio_host(host: &str) -> bool {
    host.ends_with(".music.126.net")
        || host.ends_with(".126.net")
        || host.ends_with(".netease.com")
        || host.ends_with(".127.net")
}

#[derive(Clone)]
pub struct AppState {
    pub cookies: Arc<Mutex<Option<String>>>,
    pub account: Arc<Mutex<Option<Account>>>,
    pub qr_jars: Arc<Mutex<BTreeMap<String, BTreeMap<String, String>>>>,
    pub url_cache: Arc<Mutex<HashMap<String, (String, Instant)>>>,
    pub session_epoch: Arc<Mutex<u64>>,
    pub qr_poll_lock: Arc<tokio::sync::Mutex<()>>,
    pub api: reqwest::Client,
    pub stream: reqwest::Client,
    pub proxy: ProxyInfo,
}

impl AppState {
    pub fn new(proxy: ProxyInfo) -> Self {
        let api = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::limited(8))
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        // Audio responses are long-lived; a total request timeout would cut the
        // stream off after a few seconds. Only cap connection establishment.
        let stream = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.url().host_str().is_some_and(is_allowed_audio_host) {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            cookies: Arc::new(Mutex::new(None)),
            account: Arc::new(Mutex::new(None)),
            qr_jars: Arc::new(Mutex::new(BTreeMap::new())),
            url_cache: Arc::new(Mutex::new(HashMap::new())),
            session_epoch: Arc::new(Mutex::new(0)),
            qr_poll_lock: Arc::new(tokio::sync::Mutex::new(())),
            api,
            stream,
            proxy,
        }
    }

    pub fn cookie_header(&self) -> Option<String> {
        self.cookies
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn account(&self) -> Option<Account> {
        self.account.lock().ok().and_then(|slot| slot.clone())
    }

    pub fn session_epoch(&self) -> u64 {
        self.session_epoch.lock().map(|epoch| *epoch).unwrap_or(0)
    }

    pub fn set_session(&self, cookies: Option<String>, account: Option<Account>) {
        if let Ok(mut epoch) = self.session_epoch.lock() {
            *epoch = epoch.wrapping_add(1);
        }
        if let Ok(mut slot) = self.cookies.lock() {
            *slot = cookies;
        }
        if let Ok(mut slot) = self.account.lock() {
            *slot = account;
        }
        if let Ok(mut cache) = self.url_cache.lock() {
            cache.clear();
        }
    }
}

pub fn cookie_value(cookies: &str, name: &str) -> Option<String> {
    cookies.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.trim().to_string())
    })
}
