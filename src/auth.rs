use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::secure_store;
use crate::state::{Account, AppState, API_BASE};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QrStart {
    pub unikey: String,
    pub qr_url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QrPoll {
    pub code: i32,
    pub message: String,
    pub account: Option<Account>,
}

fn auth_path() -> Result<PathBuf, String> {
    let path = crate::paths::auth_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("无法创建应用数据目录: {e}"))?;
    }
    Ok(path)
}

fn read_cookie_file(path: &Path) -> Option<String> {
    let encrypted = fs::read(path).ok()?;
    let plain = secure_store::unprotect(&encrypted).ok()?;
    String::from_utf8(plain)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn load_cookies() -> Option<String> {
    let path = auth_path().ok()?;
    read_cookie_file(&path)
}

fn save_cookies(cookies: &str) -> Result<(), String> {
    let path = auth_path()?;
    let encrypted = secure_store::protect(cookies.as_bytes())?;
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&temporary, encrypted).map_err(|e| {
        let _ = fs::remove_file(&temporary);
        format!("写入登录凭证失败: {e}")
    })?;
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("写入登录凭证失败: {error}"));
    }
    Ok(())
}

fn clear_cookies() -> Result<(), String> {
    let path = auth_path()?;
    if path.exists() {
        fs::remove_file(path).map_err(|e| format!("清除登录凭证失败: {e}"))?;
    }
    Ok(())
}

fn cookie_map(cookies: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for part in cookies.split(';') {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        if !name.trim().is_empty() && !value.trim().is_empty() {
            map.insert(name.trim().to_string(), value.trim().to_string());
        }
    }
    map
}

async fn refresh_session_cookies(state: &AppState, cookies: &str) -> String {
    let mut jar = cookie_map(cookies);
    if let Ok(response) = state
        .api
        .get(API_BASE)
        .header(reqwest::header::COOKIE, cookies)
        .header(reqwest::header::REFERER, format!("{API_BASE}/"))
        .send()
        .await
    {
        merge_set_cookies(response.headers(), &mut jar);
    }
    let merged = cookie_header(&jar);
    if merged.is_empty() {
        cookies.to_string()
    } else {
        merged
    }
}

fn cookie_header(map: &BTreeMap<String, String>) -> String {
    map.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn merge_set_cookies(headers: &reqwest::header::HeaderMap, jar: &mut BTreeMap<String, String>) {
    for value in headers.get_all(reqwest::header::SET_COOKIE) {
        let Ok(raw) = value.to_str() else { continue };
        let Some(pair) = raw.split(';').next() else {
            continue;
        };
        let Some((name, val)) = pair.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let val = val.trim();
        if name.is_empty() {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        let expired = val.is_empty()
            || lower.contains("max-age=0")
            || lower.contains("expires=thu, 01 jan 1970");
        if expired {
            jar.remove(name);
        } else {
            jar.insert(name.to_string(), val.to_string());
        }
    }
}

fn account_from_json(json: &Value) -> Option<Account> {
    let profile = json.get("profile").unwrap_or(json);
    let user_id = profile.get("userId").map(|value| match value {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        _ => String::new(),
    })?;
    if user_id.is_empty() {
        return None;
    }
    Some(Account {
        user_id,
        nickname: profile
            .get("nickname")
            .and_then(Value::as_str)
            .unwrap_or("网易云用户")
            .to_string(),
        avatar_url: profile
            .get("avatarUrl")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

async fn fetch_account(state: &AppState, cookies: &str) -> Result<Option<Account>, String> {
    let response = state
        .api
        .get(format!("{API_BASE}/api/nuser/account/get"))
        .header(reqwest::header::COOKIE, cookies)
        .header(reqwest::header::REFERER, format!("{API_BASE}/"))
        .send()
        .await
        .map_err(|e| format!("读取账号信息失败: {e}"))?;

    let json: Value = response
        .json()
        .await
        .map_err(|e| format!("账号信息格式异常: {e}"))?;

    Ok(account_from_json(&json))
}

async fn fetch_account_with_retry(state: &AppState, cookies: &str) -> Option<Account> {
    for attempt in 0..3 {
        match fetch_account(state, cookies).await {
            Ok(Some(account)) => return Some(account),
            Ok(None) | Err(_) if attempt < 2 => {
                tokio::time::sleep(std::time::Duration::from_millis(350)).await;
            }
            _ => break,
        }
    }
    None
}

fn normalize_cookie_input(raw: &str) -> Result<String, String> {
    if raw.len() > 8192 {
        return Err("登录凭证过长".to_string());
    }
    const ALLOWED: &[&str] = &[
        "MUSIC_U",
        "__csrf",
        "NMTID",
        "MUSIC_A",
        "MUSIC_A_T",
        "MUSIC_R_T",
        "_ntes_nnid",
        "_ntes_nuid",
        "JSESSIONID-WYYY",
        "WNMCID",
        "WEVNSM",
        "_iuqxldmzr_",
        "WM_NI",
        "WM_NIKE",
        "WM_TID",
    ];

    let trimmed = raw.trim();
    let trimmed = trimmed
        .strip_prefix("Cookie:")
        .or_else(|| trimmed.strip_prefix("cookie:"))
        .unwrap_or(trimmed)
        .trim()
        .replace(['\r', '\n'], ";");
    let mut seen = BTreeSet::new();
    let mut pairs = Vec::new();
    for part in trimmed.split(';') {
        let part = part.trim();
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        let Some(canonical) = ALLOWED
            .iter()
            .find(|allowed| allowed.eq_ignore_ascii_case(name))
            .copied()
        else {
            continue;
        };
        if name.is_empty()
            || value.is_empty()
            || value.chars().any(char::is_control)
            || !seen.insert(canonical.to_string())
        {
            continue;
        }
        pairs.push(format!("{canonical}={value}"));
    }

    let normalized = pairs.join("; ");
    let has_auth_cookie = normalized.split(';').any(|part| {
        let Some((name, value)) = part.trim().split_once('=') else {
            return false;
        };
        (name.eq_ignore_ascii_case("MUSIC_U") || name.eq_ignore_ascii_case("MUSIC_A"))
            && !value.trim().is_empty()
    });
    if !has_auth_cookie {
        return Err(
            "凭证中缺少 MUSIC_U/MUSIC_A，请从已登录的网页版复制 MUSIC_U 与 __csrf".to_string(),
        );
    }
    Ok(normalized)
}

pub async fn qr_login_start(state: &AppState) -> Result<QrStart, String> {
    let _guard = state.qr_poll_lock.lock().await;
    let (unikey, qr_url) = tokio::task::spawn_blocking(crate::libnetease::qr_start)
        .await
        .map_err(|e| format!("登录线程失败: {e}"))??;
    crate::logger::log(
        "debug",
        "auth",
        &format!("qr start key_len={} url_host=music.163.com", unikey.len()),
    );
    Ok(QrStart { unikey, qr_url })
}

fn poll_message(code: i32, raw: &str) -> String {
    match code {
        800 => "二维码已过期，请刷新".to_string(),
        -462 => "网易要求完成人机验证，请更换网络后重试".to_string(),
        8821 => "网易要求升级登录方式，请更换网络后重试".to_string(),
        8810 => "当前网络存在安全风险，请更换网络后重试".to_string(),
        -460 => "检测到网络环境风险，请稍后再试".to_string(),
        _ if !raw.is_empty() => raw.to_string(),
        _ => format!("登录状态异常（{code}），请刷新二维码重试"),
    }
}

fn has_auth_cookie(cookies: &str) -> bool {
    cookies.split(';').any(|part| {
        let Some((name, value)) = part.trim().split_once('=') else {
            return false;
        };
        (name.eq_ignore_ascii_case("MUSIC_U") || name.eq_ignore_ascii_case("MUSIC_A"))
            && !value.trim().is_empty()
    })
}

fn has_csrf_cookie(cookies: &str) -> bool {
    cookies.split(';').any(|part| {
        let Some((name, value)) = part.trim().split_once('=') else {
            return false;
        };
        name.eq_ignore_ascii_case("__csrf") && !value.trim().is_empty()
    })
}

pub async fn qr_login_poll(state: &AppState, key: String) -> Result<QrPoll, String> {
    let _guard = state.qr_poll_lock.lock().await;
    let key_for_call = key.clone();
    let (code, body) =
        tokio::task::spawn_blocking(move || crate::libnetease::qr_check(&key_for_call))
            .await
            .map_err(|e| format!("登录线程失败: {e}"))??;
    let json: Value = serde_json::from_str(&body).map_err(|e| format!("登录状态格式异常: {e}"))?;
    let raw_message = json.get("message").and_then(Value::as_str).unwrap_or("");
    crate::logger::log(
        "debug",
        "auth",
        &format!("qr poll code={code} body_len={}", body.len()),
    );

    if code == 803 {
        let epoch = state.session_epoch();
        // The QR success response sometimes carries the cookie jar in its
        // body instead of Set-Cookie headers. Merge that fallback first.
        if let Some(body_cookie) = json
            .get("cookie")
            .or_else(|| json.pointer("/data/cookie"))
            .and_then(Value::as_str)
        {
            if !body_cookie.trim().is_empty() {
                crate::libnetease::merge_cookies(body_cookie);
            }
        }
        let mut cookies = crate::libnetease::cookie_header();
        if !has_auth_cookie(&cookies) {
            return Err("扫码成功，但未获取到 MUSIC_U/MUSIC_A 登录凭证，请重试".to_string());
        }
        if !has_csrf_cookie(&cookies) {
            cookies = refresh_session_cookies(&state, &cookies).await;
            crate::libnetease::set_cookies(&cookies);
            if !has_csrf_cookie(&cookies) {
                return Err("登录成功，但未获取到 __csrf，请重试".to_string());
            }
        }
        let account = fetch_account_with_retry(&state, &cookies)
            .await
            .or_else(|| account_from_json(&json));
        let Some(account) = account else {
            // Do not save a half-valid session or leave the main loop
            // polling forever when the profile endpoint failed.
            return Err("扫码成功，但读取账号资料失败，请重试".to_string());
        };
        if state.session_epoch() != epoch {
            return Ok(QrPoll {
                code,
                message: "登录状态已变化，请重新扫码".to_string(),
                account: None,
            });
        }
        let _ = save_cookies(&cookies);
        state.set_session(Some(cookies), Some(account.clone()));
        return Ok(QrPoll {
            code,
            message: "登录成功，正在同步你的音乐库".to_string(),
            account: Some(account),
        });
    }

    Ok(QrPoll {
        code,
        message: poll_message(code, raw_message),
        account: None,
    })
}

pub async fn import_cookie(state: &AppState, cookie: String) -> Result<Account, String> {
    let _guard = state.qr_poll_lock.lock().await;
    let epoch = state.session_epoch();
    let cookies = normalize_cookie_input(&cookie)?;
    let cookies = if has_csrf_cookie(&cookies) {
        cookies
    } else {
        refresh_session_cookies(&state, &cookies).await
    };
    if !has_auth_cookie(&cookies) || !has_csrf_cookie(&cookies) {
        return Err("凭证已失效，或不是有效的网易云网页版 Cookie".to_string());
    }
    let account = fetch_account_with_retry(&state, &cookies)
        .await
        .ok_or_else(|| "凭证已失效，或不是有效的网易云网页版 Cookie".to_string())?;
    if state.session_epoch() != epoch {
        return Err("登录状态已变化，请重试".to_string());
    }
    // Persistence may be unavailable on non-Windows dev builds; the session
    // still works in memory rather than making the import fail outright.
    let _ = save_cookies(&cookies);
    crate::libnetease::set_cookies(&cookies);
    crate::logger::log(
        "info",
        "auth",
        &format!("cookie import ok user_id={}", account.user_id),
    );
    state.set_session(Some(cookies), Some(account.clone()));
    Ok(account)
}

pub async fn auth_status(state: &AppState) -> Result<Option<Account>, String> {
    if let Some(account) = state.account() {
        if state.cookie_header().is_some() {
            return Ok(Some(account));
        }
    }

    let Some(cookies) = load_cookies() else {
        return Ok(None);
    };
    let epoch = state.session_epoch();
    let mut account = None;
    let mut last_error = None;
    for attempt in 0..2 {
        match fetch_account(&state, &cookies).await {
            Ok(Some(value)) => {
                account = Some(value);
                break;
            }
            Ok(None) => break,
            Err(error) => {
                last_error = Some(error);
                if attempt == 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
                }
            }
        }
    }
    let Some(account) = account else {
        if let Some(error) = last_error {
            return Err(error);
        }
        return Ok(None);
    };
    if state.session_epoch() != epoch {
        return Ok(None);
    }
    crate::libnetease::set_cookies(&cookies);
    crate::logger::log("info", "auth", "startup session restored");
    state.set_session(Some(cookies), Some(account.clone()));
    Ok(Some(account))
}

pub async fn logout(state: &AppState) -> Result<(), String> {
    // Clear the in-memory session first so a failed file deletion can never
    // leave the app half-logged-in.
    let file_result = clear_cookies();
    crate::libnetease::clear_cookies();
    state.set_session(None, None);
    crate::logger::log("info", "auth", "logout completed");
    file_result
}

#[cfg(test)]
mod tests {
    use super::normalize_cookie_input;

    #[test]
    fn accepts_music_u() {
        assert_eq!(
            normalize_cookie_input("Cookie: MUSIC_U=abc; __csrf=def").unwrap(),
            "MUSIC_U=abc; __csrf=def"
        );
    }

    #[test]
    fn rejects_missing_music_u() {
        assert!(normalize_cookie_input("__csrf=def").is_err());
    }
}
