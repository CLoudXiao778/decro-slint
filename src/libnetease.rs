//! Safe-ish FFI boundary around the original Cirro `libnetease` C library.
//!
//! The library provides the weapi/eapi signing and cookie jar that NetEase
//! expects; Decro injects a reqwest-backed transport instead of libcurl.
//! Only the endpoints Decro needs (QR login, song URL, like) are wrapped here.

use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use libc::c_long;
use rand::Rng;
use serde_json::Value;

const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
const MAC_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";

#[repr(C)]
struct NeHttpResp {
    status: c_long,
    body: *mut c_char,
    body_len: usize,
    err: *mut c_char,
    set_cookies: *mut c_char,
}

#[repr(C)]
struct NeHttpTransport {
    request: Option<
        unsafe extern "C" fn(
            url: *const c_char,
            method: *const c_char,
            body: *const c_char,
            content_type: *const c_char,
            cookie_header: *const c_char,
            user_agent: *const c_char,
        ) -> *mut NeHttpResp,
    >,
}

#[repr(C)]
struct NeResp {
    code: f64,
    http_status: c_long,
    body: *mut c_char,
    body_len: usize,
    err: i32,
}

#[repr(C)]
struct Jmap {
    _private: [u8; 0],
}

extern "C" {
    fn ne_http_set_transport(transport: *const NeHttpTransport);
    fn ne_set_api_base(base: *const c_char);
    fn ne_set_cookie_file(path: *const c_char);
    fn ne_jar_reload();
    fn ne_jar_import_cookies(cookie_str: *const c_char);
    fn ne_global_cookie_header() -> *mut c_char;

    fn ne_qr_get_key(
        code_out: *mut f64,
        body_out: *mut *mut c_char,
        body_len: *mut usize,
    ) -> *mut c_char;
    fn ne_qr_check(unikey: *const c_char, code_out: *mut f64, body_len: *mut usize) -> *mut c_char;
    fn ne_qr_build_url(unikey: *const c_char) -> *mut c_char;

    fn ne_song_url_v1(id: *const c_char, level: *const c_char) -> *mut NeResp;
    fn ne_song_like(track_id: *const c_char, like: *const c_char) -> *mut NeResp;
    fn ne_eapi(url: *const c_char, data: *const Jmap) -> *mut c_char;
    fn jmap_new() -> *mut Jmap;
    fn jmap_put(map: *mut Jmap, key: *const c_char, value: *const c_char);
    fn jmap_put_map(map: *mut Jmap, key: *const c_char, value: *mut Jmap);
    fn jmap_free(map: *mut Jmap);
    fn ne_album_detail(id: *const c_char) -> *mut NeResp;
    fn ne_playlist_subscribe(id: *const c_char, t: *const c_char) -> *mut NeResp;
    fn ne_playlist_create(name: *const c_char, privacy: *const c_char) -> *mut NeResp;
    fn ne_homepage_block_page(refresh: *const c_char, cursor: *const c_char) -> *mut NeResp;
    fn ne_resp_free(resp: *mut NeResp);
    fn decro_alloc(size: usize) -> *mut c_void;
    fn decro_free(ptr: *mut c_void);
}

static TRANSPORT: NeHttpTransport = NeHttpTransport {
    request: Some(transport_request),
};

static INIT: OnceLock<()> = OnceLock::new();
static CRYPTO_LOCK: Mutex<()> = Mutex::new(());
static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

fn cstring(value: &str) -> CString {
    CString::new(value).unwrap_or_else(|_| CString::new("").expect("empty CString"))
}

fn cookie_path() -> CString {
    // Keep libnetease's jar in memory only. The DPAPI file owned by auth.rs is
    // the single persistent copy of the session.
    cstring(if cfg!(windows) { "NUL" } else { "/dev/null" })
}

pub fn init() {
    INIT.get_or_init(|| {
        // Browser-like headers reduce HTTP fingerprint differences without
        // faking an IP or bypassing any verification.
        std::env::set_var("NE_BROWSER_HEADERS", "1");
        unsafe {
            ne_set_api_base(cstring("https://music.163.com").as_ptr());
            ne_set_cookie_file(cookie_path().as_ptr());
            ne_http_set_transport(&TRANSPORT);
            // Risk-control retries are deliberately left off: the same switch
            // also retries non-idempotent writes such as playlist creation.
        }
    });
}

pub fn set_cookies(cookies: &str) {
    init();
    // The C jar has its own lock. Reload + import are both thread-safe and
    // must not be serialized with in-flight HTTP requests by the Rust layer.
    let raw = cstring(cookies);
    unsafe {
        ne_jar_reload();
        ne_jar_import_cookies(raw.as_ptr());
    }
}

/// Merge a cookie string into the live jar without clearing it first. Used for
/// the QR 803 response, whose `cookie` field is not always mirrored in
/// Set-Cookie headers.
pub fn merge_cookies(cookies: &str) {
    init();
    let raw = cstring(cookies);
    unsafe {
        ne_jar_import_cookies(raw.as_ptr());
    }
}

pub fn clear_cookies() {
    init();
    unsafe {
        ne_jar_reload();
    }
}

pub fn cookie_header() -> String {
    init();
    unsafe {
        let ptr = ne_global_cookie_header();
        take_c_string(ptr)
    }
}

fn cookie_pairs(header: &str) -> Vec<(String, String)> {
    header
        .split(';')
        .filter_map(|part| {
            let mut pieces = part.trim().splitn(2, '=');
            let key = pieces.next()?.trim();
            let value = pieces.next()?.trim();
            if key.is_empty() {
                None
            } else {
                Some((key.to_string(), value.to_string()))
            }
        })
        .collect()
}

fn cookie_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_str())
}

fn set_cookie(pairs: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some((_, current)) = pairs
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
    {
        *current = value.to_string();
    } else {
        pairs.push((key.to_string(), value.to_string()));
    }
}

fn cookie_string(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn random_device_id() -> String {
    let mut rng = rand::thread_rng();
    (0..52)
        .map(|_| format!("{:X}", rng.gen_range(0..16u8)))
        .collect()
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Report a "startplay" event to NetEase's desktop log endpoint. This is what
/// updates the account's 最近播放 list; the old `/weapi/feedback/weblog`
/// endpoint returns success but is silently discarded by the server.
pub fn report_play_start(id: &str, source_id: &str) -> Result<(), String> {
    init();
    if id.is_empty() || !id.chars().all(|character| character.is_ascii_digit()) {
        return Err("无效的歌曲 ID".to_string());
    }

    let raw_cookie = cookie_header();
    if raw_cookie.trim().is_empty() {
        return Err("请先登录网易云".to_string());
    }
    let mut cookies = cookie_pairs(&raw_cookie);
    let music_u = cookie_value(&cookies, "MUSIC_U").unwrap_or("").to_string();
    let music_a = cookie_value(&cookies, "MUSIC_A").unwrap_or("").to_string();
    if music_u.is_empty() && music_a.is_empty() {
        return Err("请先登录网易云".to_string());
    }

    let now = unix_seconds();
    let mut rng = rand::thread_rng();
    let device_id = cookie_value(&cookies, "deviceId")
        .or_else(|| cookie_value(&cookies, "sDeviceId"))
        .map(str::to_string)
        .unwrap_or_else(random_device_id);
    let csrf = cookie_value(&cookies, "__csrf").unwrap_or("").to_string();
    let buildver = now.to_string();
    let request_id = format!("{}{:03}", now.saturating_mul(1000), rng.gen_range(0..1000));
    let source_id = if source_id.is_empty() { "0" } else { source_id };

    let overrides = [
        ("os", "osx"),
        ("appver", "3.1.10.5100"),
        ("osver", "15.5"),
        ("channel", "netease"),
        ("deviceId", device_id.as_str()),
        ("versioncode", "140"),
        ("mobilename", ""),
        ("buildver", buildver.as_str()),
        ("resolution", "1920x1080"),
        ("__csrf", csrf.as_str()),
        ("requestId", request_id.as_str()),
    ];
    for (key, value) in overrides {
        set_cookie(&mut cookies, key, value);
    }

    let track_id = id.parse::<u64>().map_err(|_| "无效的歌曲 ID".to_string())?;
    let logs = serde_json::json!([{
        "action": "startplay",
        "json": {
            "id": track_id,
            "type": "song",
            "mainsite": "1",
            "mainsiteWeb": "1",
            "content": format!("id={source_id}")
        }
    }])
    .to_string();

    let mut header = vec![
        ("osver".to_string(), "15.5".to_string()),
        ("deviceId".to_string(), device_id.clone()),
        ("appver".to_string(), "3.1.10.5100".to_string()),
        ("versioncode".to_string(), "140".to_string()),
        ("mobilename".to_string(), String::new()),
        ("buildver".to_string(), buildver),
        ("resolution".to_string(), "1920x1080".to_string()),
        ("__csrf".to_string(), csrf),
        ("os".to_string(), "osx".to_string()),
        ("channel".to_string(), "netease".to_string()),
        ("requestId".to_string(), request_id),
    ];
    if !music_u.is_empty() {
        header.push(("MUSIC_U".to_string(), music_u));
    }
    if !music_a.is_empty() {
        header.push(("MUSIC_A".to_string(), music_a));
    }

    let params = {
        // The C eapi/JMap helpers are not documented as thread-safe; the
        // encryption section is short and does not perform network I/O.
        let _guard = CRYPTO_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            let data = jmap_new();
            if data.is_null() {
                return Err("播放上报初始化失败".to_string());
            }
            jmap_put(data, cstring("logs").as_ptr(), cstring(&logs).as_ptr());
            let header_map = jmap_new();
            if header_map.is_null() {
                jmap_free(data);
                return Err("播放上报初始化失败".to_string());
            }
            for (key, value) in &header {
                jmap_put(header_map, cstring(key).as_ptr(), cstring(value).as_ptr());
            }
            jmap_put_map(data, cstring("header").as_ptr(), header_map);
            let encrypted = ne_eapi(cstring("/api/feedback/weblog").as_ptr(), data);
            jmap_free(data);
            take_c_string(encrypted)
        }
    };
    if params.is_empty() {
        return Err("播放上报加密失败".to_string());
    }

    let response = client()
        .post("https://clientlog.music.163.com/eapi/feedback/weblog")
        .header(reqwest::header::REFERER, "https://music.163.com/")
        .header(reqwest::header::ORIGIN, "https://music.163.com")
        .header(reqwest::header::USER_AGENT, MAC_UA)
        .header(reqwest::header::COOKIE, cookie_string(&cookies))
        .form(&[("params", params.as_str())])
        .send()
        .map_err(|error| format!("播放上报失败: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .map_err(|error| format!("播放上报响应读取失败: {error}"))?;
    if !status.is_success() {
        return Err(format!("播放上报失败: HTTP {status}"));
    }
    let json: Value =
        serde_json::from_str(&text).map_err(|error| format!("播放上报响应格式异常: {error}"))?;
    let code = json.get("code").and_then(Value::as_i64).unwrap_or(200);
    if code != 200 {
        let message = json
            .get("message")
            .or_else(|| json.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("网易云拒绝了播放上报");
        return Err(format!("{message}（{code}）"));
    }
    crate::logger::log(
        "info",
        "play",
        &format!("reported id={id} source={source_id}"),
    );
    Ok(())
}

pub fn qr_start() -> Result<(String, String), String> {
    init();
    unsafe {
        let mut code = 0.0f64;
        let mut body_ptr = ptr::null_mut();
        let mut body_len = 0usize;
        let key_ptr = ne_qr_get_key(&mut code, &mut body_ptr, &mut body_len);
        let key = take_c_string(key_ptr);
        let body = take_c_string(body_ptr);
        crate::logger::log(
            "debug",
            "qr",
            &format!(
                "key_len={} body_len={} code={code:.0}",
                key.len(),
                body.len()
            ),
        );
        if key.is_empty() {
            return Err(qr_error(&body, code, "网易云未返回二维码 key"));
        }
        let url_ptr = ne_qr_build_url(cstring(&key).as_ptr());
        let url = take_c_string(url_ptr);
        if url.is_empty() {
            return Err("网易云未返回二维码地址".to_string());
        }
        Ok((key, url))
    }
}

pub fn qr_check(unikey: &str) -> Result<(i32, String), String> {
    init();
    unsafe {
        let mut code = 0.0f64;
        let mut body_len = 0usize;
        let body_ptr = ne_qr_check(cstring(unikey).as_ptr(), &mut code, &mut body_len);
        let body = take_c_string(body_ptr);
        crate::logger::log(
            "debug",
            "qr",
            &format!("check body_len={} code={code:.0}", body.len()),
        );
        if body.is_empty() {
            return Err("网易云登录状态响应为空".to_string());
        }
        Ok((code as i32, body))
    }
}

fn response_json(resp: *mut NeResp, target: &str) -> Result<Value, String> {
    let (code, err, body) = unsafe { take_resp(resp)? };
    if !err.is_empty() {
        return Err(err);
    }
    if body.is_empty() {
        return Err(format!("{target}响应为空"));
    }
    let json: Value =
        serde_json::from_str(&body).map_err(|e| format!("{target}响应格式异常: {e}"))?;
    if let Some(status) = json.get("code").and_then(Value::as_i64) {
        if status != 200 && status != 0 {
            let message = json
                .get("message")
                .or_else(|| json.get("msg"))
                .and_then(Value::as_str)
                .filter(|message| !message.is_empty())
                .unwrap_or("网易云拒绝了这次请求");
            return Err(format!("网易云：{message}（{status}, code={code}）"));
        }
    }
    Ok(json)
}

/// Radar playlists from the mobile homepage block stream. The block is the
/// only place NetEase exposes radar cards; the generic personalized-playlist
/// endpoint returns static curated playlists instead.
pub fn homepage_radar() -> Result<Value, String> {
    init();
    let resp = unsafe { ne_homepage_block_page(cstring("false").as_ptr(), cstring("").as_ptr()) };
    response_json(resp, "雷达歌单")
}

pub fn playlist_subscribe(id: &str, subscribe: bool) -> Result<(), String> {
    init();
    let resp = unsafe {
        ne_playlist_subscribe(
            cstring(id).as_ptr(),
            cstring(if subscribe { "1" } else { "0" }).as_ptr(),
        )
    };
    response_json(resp, "歌单收藏")?;
    crate::logger::log(
        "info",
        "playlist",
        &format!("id={id} subscribed={subscribe}"),
    );
    Ok(())
}

pub fn playlist_create(name: &str) -> Result<String, String> {
    init();
    let resp = unsafe { ne_playlist_create(cstring(name).as_ptr(), cstring("0").as_ptr()) };
    let json = response_json(resp, "新建歌单")?;
    let id = json
        .get("playlistId")
        .or_else(|| json.get("id"))
        .and_then(|value| match value {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .ok_or_else(|| "网易云没有返回新歌单 ID".to_string())?;
    crate::logger::log("info", "playlist", &format!("created id={id}"));
    Ok(id)
}

pub fn album_detail(id: &str) -> Result<Value, String> {
    init();
    let resp = unsafe { ne_album_detail(cstring(id).as_ptr()) };
    let json = response_json(resp, "专辑详情")?;
    crate::logger::log("debug", "album", &format!("id={id} detail loaded"));
    Ok(json)
}

pub fn song_url(id: &str, level: &str) -> Result<(String, bool), String> {
    init();
    unsafe {
        let resp = ne_song_url_v1(cstring(id).as_ptr(), cstring(level).as_ptr());
        let (code, err, body) = take_resp(resp)?;
        crate::logger::log(
            "debug",
            "song",
            &format!("id={id} level={level} code={code} body_len={}", body.len()),
        );
        let json: Value =
            serde_json::from_str(&body).map_err(|e| format!("音源响应格式异常: {e}"))?;
        let trial = json
            .pointer("/data/0/freeTrialInfo")
            .is_some_and(|value| !value.is_null());
        if let Some(url) = json
            .pointer("/data/0/url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
        {
            // A trial URL is still playable, but the caller must know so it
            // can show "试听" instead of the requested quality.
            return Ok((url.to_string(), trial));
        }
        if trial {
            return Err("该歌曲仅提供试听片段（需要会员）".to_string());
        }
        let inner_code = json
            .pointer("/data/0/code")
            .and_then(Value::as_i64)
            .unwrap_or(code);
        let message = json
            .pointer("/data/0/message")
            .and_then(Value::as_str)
            .filter(|message| !message.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("该歌曲暂不可播放（code={inner_code}）"));
        if !err.is_empty() {
            Err(err)
        } else {
            Err(message)
        }
    }
}

pub fn song_like(id: &str, like: bool) -> Result<(), String> {
    init();
    unsafe {
        let resp = ne_song_like(
            cstring(id).as_ptr(),
            cstring(if like { "true" } else { "false" }).as_ptr(),
        );
        let (code, err, body) = take_resp(resp)?;
        crate::logger::log("info", "like", &format!("id={id} like={like} code={code}"));
        if code == 200 {
            return Ok(());
        }
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|json| {
                json.get("message")
                    .or_else(|| json.get("msg"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| {
                if err.is_empty() {
                    format!("喜欢操作失败（code={code}）")
                } else {
                    err
                }
            });
        Err(message)
    }
}

fn qr_error(body: &str, code: f64, fallback: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|json| {
            json.get("message")
                .or_else(|| json.get("msg"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| {
            if code != 0.0 && code != 200.0 {
                format!("{fallback}（code={code:.0}）")
            } else {
                fallback.to_string()
            }
        })
}

unsafe fn take_resp(resp: *mut NeResp) -> Result<(i64, String, String), String> {
    if resp.is_null() {
        return Err("libnetease 返回空响应".to_string());
    }
    let body = if (*resp).body.is_null() {
        String::new()
    } else {
        let bytes = std::slice::from_raw_parts((*resp).body as *const u8, (*resp).body_len);
        String::from_utf8_lossy(bytes).into_owned()
    };
    let err = if (*resp).err == 0 {
        String::new()
    } else {
        let text = if (*resp).body.is_null() {
            String::new()
        } else {
            String::from_utf8_lossy(std::slice::from_raw_parts(
                (*resp).body as *const u8,
                (*resp).body_len,
            ))
            .into_owned()
        };
        text
    };
    let code = (*resp).code as i64;
    let http_status = (*resp).http_status;
    ne_resp_free(resp);
    if code == 0 && http_status == 0 && !err.is_empty() {
        return Err(err);
    }
    Ok((code, err, body))
}

unsafe fn take_c_string(ptr: *mut c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let value = CStr::from_ptr(ptr).to_string_lossy().into_owned();
    decro_free(ptr as *mut c_void);
    value
}

unsafe fn read_c_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

fn client() -> &'static reqwest::blocking::Client {
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(8))
            .timeout(std::time::Duration::from_secs(20))
            .gzip(true)
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new())
    })
}

unsafe extern "C" fn transport_request(
    url: *const c_char,
    method: *const c_char,
    body: *const c_char,
    content_type: *const c_char,
    cookie_header: *const c_char,
    user_agent: *const c_char,
) -> *mut NeHttpResp {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport_impl(url, method, body, content_type, cookie_header, user_agent)
    }));
    match result {
        Ok(response) => response,
        Err(_) => make_response(0, Vec::new(), None, Some("transport panic".to_string())),
    }
}

unsafe fn transport_impl(
    url: *const c_char,
    method: *const c_char,
    body: *const c_char,
    content_type: *const c_char,
    cookie_header: *const c_char,
    user_agent: *const c_char,
) -> *mut NeHttpResp {
    let url = read_c_string(url);
    if url.is_empty() {
        return make_response(0, Vec::new(), None, Some("empty transport URL".to_string()));
    }
    let method = read_c_string(method);
    let safe_url = url.split('?').next().unwrap_or(&url).to_string();
    let body = read_c_string(body);
    let content_type = read_c_string(content_type);
    let cookie = read_c_string(cookie_header);
    let agent = read_c_string(user_agent);

    crate::logger::log(
        "debug",
        "http",
        &format!("{} {} body_len={}", method, safe_url, body.len()),
    );
    let request_method = if method.eq_ignore_ascii_case("GET") {
        reqwest::Method::GET
    } else {
        reqwest::Method::POST
    };
    let mut request = client()
        .request(request_method, &url)
        .header(reqwest::header::REFERER, "https://music.163.com/")
        .header(reqwest::header::ACCEPT, "*/*")
        .header(
            reqwest::header::USER_AGENT,
            if agent.is_empty() { USER_AGENT } else { &agent },
        );
    if !cookie.is_empty() {
        request = request.header(reqwest::header::COOKIE, cookie);
    }
    if !body.is_empty() {
        request = request.header(
            reqwest::header::CONTENT_TYPE,
            if content_type.is_empty() {
                "application/x-www-form-urlencoded"
            } else {
                content_type.as_str()
            },
        );
        request = request.body(body);
    }

    match request.send() {
        Ok(response) => {
            let status = response.status().as_u16() as c_long;
            crate::logger::log(
                "debug",
                "http",
                &format!("{} {} status={}", method, safe_url, status),
            );
            let set_cookies = response
                .headers()
                .get_all(reqwest::header::SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            match response.bytes() {
                Ok(bytes) => make_response(
                    status,
                    bytes.to_vec(),
                    if set_cookies.is_empty() {
                        None
                    } else {
                        Some(set_cookies)
                    },
                    None,
                ),
                Err(error) => make_response(
                    status,
                    Vec::new(),
                    None,
                    Some(format!("response read failed: {error}")),
                ),
            }
        }
        Err(error) => make_response(
            0,
            Vec::new(),
            None,
            Some(format!("transport request failed: {error}")),
        ),
    }
}

fn make_response(
    status: c_long,
    body: Vec<u8>,
    set_cookies: Option<String>,
    err: Option<String>,
) -> *mut NeHttpResp {
    unsafe {
        let body_ptr = alloc_bytes(&body);
        let err_ptr = err
            .as_deref()
            .map(|value| alloc_c_string(value))
            .unwrap_or(ptr::null_mut());
        let cookies_ptr = set_cookies
            .as_deref()
            .map(|value| alloc_c_string(value))
            .unwrap_or(ptr::null_mut());
        let response = decro_alloc(std::mem::size_of::<NeHttpResp>()) as *mut NeHttpResp;
        if response.is_null() {
            decro_free(body_ptr as *mut c_void);
            decro_free(err_ptr as *mut c_void);
            decro_free(cookies_ptr as *mut c_void);
            return ptr::null_mut();
        }
        ptr::write(
            response,
            NeHttpResp {
                status,
                body: body_ptr,
                body_len: body.len(),
                err: err_ptr,
                set_cookies: cookies_ptr,
            },
        );
        response
    }
}

unsafe fn alloc_bytes(bytes: &[u8]) -> *mut c_char {
    let ptr = decro_alloc(bytes.len() + 1) as *mut u8;
    if ptr.is_null() {
        return ptr::null_mut();
    }
    ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
    *ptr.add(bytes.len()) = 0;
    ptr as *mut c_char
}

unsafe fn alloc_c_string(value: &str) -> *mut c_char {
    alloc_bytes(value.as_bytes())
}
