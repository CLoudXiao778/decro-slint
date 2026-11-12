use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::state::{AppState, API_BASE};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackDto {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub artist_id: String,
    pub artists: Vec<ArtistRef>,
    pub album: String,
    pub album_id: String,
    pub duration_ms: u64,
    pub cover_url: String,
    pub fee: i64,
    pub quality: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDto {
    pub id: String,
    pub name: String,
    pub cover_url: String,
    pub track_count: u64,
    pub play_count: u64,
    pub description: String,
    pub creator: String,
    pub special_type: i64,
    pub update_time: u64,
    pub subscribed: bool,
    pub tracks: Vec<TrackDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeFeed {
    pub recommended: Vec<PlaylistDto>,
    pub radar: Vec<PlaylistDto>,
    pub new_songs: Vec<TrackDto>,
    pub daily: Vec<TrackDto>,
    pub recent: Vec<TrackDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryData {
    pub playlists: Vec<PlaylistDto>,
    pub recent: Vec<TrackDto>,
    pub liked: Option<PlaylistDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchData {
    pub songs: Vec<TrackDto>,
    pub playlists: Vec<PlaylistDto>,
    pub artists: Vec<ArtistInfo>,
    pub albums: Vec<AlbumInfo>,
    pub lyrics: Vec<LyricSearchDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricSearchDto {
    pub track: TrackDto,
    pub lines: Vec<String>,
}

impl SearchData {
    fn empty() -> Self {
        Self {
            songs: Vec::new(),
            playlists: Vec::new(),
            artists: Vec::new(),
            albums: Vec::new(),
            lyrics: Vec::new(),
            error: None,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricLine {
    pub time: f64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistInfo {
    pub id: String,
    pub name: String,
    pub avatar_url: String,
    pub brief_desc: String,
    pub aliases: Vec<String>,
    pub album_count: u64,
    pub song_count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumInfo {
    pub id: String,
    pub name: String,
    pub cover_url: String,
    pub publish_time: u64,
    pub size: u64,
    pub company: String,
    pub kind: String,
    pub artist: String,
    pub artist_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistPageData {
    pub artist: ArtistInfo,
    pub songs: Vec<TrackDto>,
    pub albums: Vec<AlbumInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumPageData {
    pub album: AlbumInfo,
    pub description: String,
    pub artists: Vec<ArtistRef>,
    pub songs: Vec<TrackDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricData {
    pub lines: Vec<LyricLine>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LikeResult {
    pub liked: bool,
}

async fn get_json(state: &AppState, path: &str, query: &[(&str, String)]) -> Result<Value, String> {
    let mut request = state
        .api
        .get(format!("{API_BASE}{path}"))
        .query(query)
        .header(reqwest::header::REFERER, format!("{API_BASE}/"));
    if let Some(cookies) = state.cookie_header() {
        request = request.header(reqwest::header::COOKIE, cookies);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("网易云请求失败: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("网易云接口返回 HTTP {}", response.status()));
    }
    let json: Value = response
        .json()
        .await
        .map_err(|e| format!("网易云响应格式异常: {e}"))?;
    check_code(&json)?;
    Ok(json)
}

fn check_code(json: &Value) -> Result<(), String> {
    let Some(code) = json.get("code").and_then(Value::as_i64) else {
        return Ok(());
    };
    if code == 200 || code == 0 {
        return Ok(());
    }
    let message = json
        .get("message")
        .or_else(|| json.get("msg"))
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .unwrap_or("网易云拒绝了这次请求，请稍后重试");
    Err(format!("网易云：{message}（{code}）"))
}

fn value_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// NetEase mixes integer and floating-point numbers (play counts are often
/// `2.11E7`). Accept both instead of silently turning them into 0.
fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|number| number.max(0.0) as u64))
}

fn sized_cover(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return String::new();
    }
    let url = https_url(url);
    if url.contains('?') {
        format!("{url}&param=500y500")
    } else {
        format!("{url}?param=500y500")
    }
}

fn https_url(url: &str) -> String {
    url.strip_prefix("http://")
        .map(|rest| format!("https://{rest}"))
        .unwrap_or_else(|| url.to_string())
}

fn has_object(value: &Value, key: &str) -> bool {
    value.get(key).is_some_and(|item| !item.is_null())
}

fn first_artist_id(value: &Value) -> String {
    value
        .get("ar")
        .or_else(|| value.get("artists"))
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .and_then(|artist| artist.get("id"))
        .and_then(value_string)
        .unwrap_or_default()
}

fn normalize_artist(value: &Value) -> Option<ArtistInfo> {
    let id = value.get("id").and_then(value_string)?;
    let aliases = value
        .get("alias")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|alias| !alias.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(ArtistInfo {
        id,
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("未知歌手")
            .to_string(),
        avatar_url: value
            .get("picUrl")
            .or_else(|| value.get("img1v1Url"))
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .map(sized_cover)
            .unwrap_or_default(),
        brief_desc: value
            .get("briefDesc")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        aliases,
        album_count: value.get("albumSize").and_then(value_u64).unwrap_or(0),
        song_count: value.get("musicSize").and_then(value_u64).unwrap_or(0),
    })
}

fn normalize_album(
    value: &Value,
    fallback_artist_id: &str,
    fallback_artist: &str,
) -> Option<AlbumInfo> {
    let id = value.get("id").and_then(value_string)?;
    let artists = value.get("artists").and_then(Value::as_array);
    let artist = artists
        .map(|list| {
            list.iter()
                .filter_map(|artist| artist.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| fallback_artist.to_string());
    let artist_id = artists
        .and_then(|list| list.first())
        .and_then(|artist| artist.get("id"))
        .and_then(value_string)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| fallback_artist_id.to_string());
    Some(AlbumInfo {
        id,
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("未命名专辑")
            .to_string(),
        cover_url: value
            .get("picUrl")
            .or_else(|| value.get("blurPicUrl"))
            .and_then(Value::as_str)
            .map(sized_cover)
            .unwrap_or_default(),
        publish_time: value.get("publishTime").and_then(value_u64).unwrap_or(0),
        size: value.get("size").and_then(value_u64).unwrap_or(0),
        company: value
            .get("company")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        kind: value
            .get("subType")
            .or_else(|| value.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("专辑")
            .to_string(),
        artist,
        artist_id,
    })
}

pub fn normalize_song(value: &Value) -> Option<TrackDto> {
    let id = value.get("id").and_then(value_string)?;
    let name = value
        .get("name")
        .or_else(|| value.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("未知歌曲")
        .to_string();

    let artist_list = value
        .get("ar")
        .or_else(|| value.get("artists"))
        .and_then(Value::as_array);
    let artists = artist_list
        .map(|list| {
            list.iter()
                .filter_map(|artist| artist.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "未知艺人".to_string());
    let artist_refs = artist_list
        .map(|list| {
            list.iter()
                .filter_map(|artist| {
                    let id = artist.get("id").and_then(value_string)?;
                    let name = artist.get("name").and_then(Value::as_str)?;
                    Some(ArtistRef {
                        id,
                        name: name.to_string(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let album_value = value.get("al").or_else(|| value.get("album"));
    let album = album_value
        .and_then(|album| album.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("未知专辑")
        .to_string();
    let cover = album_value
        .and_then(|album| {
            album
                .get("picUrl")
                .or_else(|| album.get("blurPicUrl"))
                .and_then(Value::as_str)
        })
        .or_else(|| value.get("picUrl").and_then(Value::as_str))
        .map(sized_cover)
        .unwrap_or_default();

    let artist_id = first_artist_id(value);
    let album_id = album_value
        .and_then(|album| album.get("id"))
        .and_then(value_string)
        .unwrap_or_default();
    let duration_ms = value
        .get("dt")
        .or_else(|| value.get("duration"))
        .and_then(value_u64)
        .unwrap_or(0);
    let fee = value.get("fee").and_then(Value::as_i64).unwrap_or(0);

    let quality = if has_object(value, "hr") || has_object(value, "hrMusic") {
        "Hi-Res"
    } else if has_object(value, "sq") || has_object(value, "sqMusic") {
        "SQ"
    } else if has_object(value, "h") || has_object(value, "hMusic") {
        "HQ"
    } else {
        "标准"
    }
    .to_string();

    Some(TrackDto {
        id,
        name,
        artist: artists,
        artist_id,
        artists: artist_refs,
        album,
        album_id,
        duration_ms,
        cover_url: cover,
        fee,
        quality,
    })
}

fn normalize_tracks(value: Option<&Value>) -> Vec<TrackDto> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(normalize_song).collect())
        .unwrap_or_default()
}

pub fn normalize_playlist(value: &Value) -> Option<PlaylistDto> {
    let id = value.get("id").and_then(value_string)?;
    let tracks = normalize_tracks(value.get("tracks"));
    let track_count = value
        .get("trackCount")
        .and_then(value_u64)
        .unwrap_or(tracks.len() as u64);
    let play_count = value.get("playCount").and_then(value_u64).unwrap_or(0);
    let cover_url = value
        .get("coverImgUrl")
        .or_else(|| value.get("picUrl"))
        .and_then(Value::as_str)
        .map(sized_cover)
        .unwrap_or_default();
    let creator = value
        .get("creator")
        .and_then(|creator| creator.get("nickname"))
        .and_then(Value::as_str)
        .unwrap_or("网易云音乐")
        .to_string();
    let description = value
        .get("description")
        .or_else(|| value.get("copywriter"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let special_type = value
        .get("specialType")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let update_time = value.get("updateTime").and_then(value_u64).unwrap_or(0);
    let subscribed = value
        .get("subscribed")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    Some(PlaylistDto {
        id,
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("未命名歌单")
            .to_string(),
        cover_url,
        track_count,
        play_count,
        description,
        creator,
        special_type,
        update_time,
        subscribed,
        tracks,
    })
}

fn playlists_from(value: Option<&Value>) -> Vec<PlaylistDto> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(normalize_playlist).collect())
        .unwrap_or_default()
}

fn parse_daily(value: &Value) -> Vec<TrackDto> {
    let daily = normalize_tracks(value.pointer("/data/dailySongs"));
    if !daily.is_empty() {
        return daily;
    }
    value
        .get("recommend")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(normalize_song).collect())
        .unwrap_or_default()
}

fn parse_recent(value: &Value) -> Vec<TrackDto> {
    let list = value
        .get("allData")
        .or_else(|| value.get("weekData"))
        .and_then(Value::as_array);
    list.map(|items| {
        items
            .iter()
            .map(|item| item.get("song").unwrap_or(item))
            .filter_map(normalize_song)
            .collect()
    })
    .unwrap_or_default()
}

fn parse_recent_song_list(value: &Value) -> Vec<TrackDto> {
    let list = value
        .get("data")
        .and_then(|data| {
            data.get("list")
                .and_then(Value::as_array)
                .or_else(|| data.as_array())
        })
        .or_else(|| value.get("list").and_then(Value::as_array));
    list.map(|items| {
        items
            .iter()
            .filter_map(|item| {
                let song = item
                    .get("song")
                    .or_else(|| item.get("data"))
                    .or_else(|| item.get("track"))
                    .unwrap_or(item);
                normalize_song(song)
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Recent play history: prefer the chronological endpoint, fall back to the
/// all-time play ranking (the week-only ranking is too short for this list).
pub async fn recent_played(state: &AppState) -> Result<Vec<TrackDto>, String> {
    match get_json(
        state,
        "/api/play-record/song/list",
        &[("limit", "100".to_string())],
    )
    .await
    {
        Ok(value) => {
            let list = parse_recent_song_list(&value);
            crate::logger::log(
                "debug",
                "recent",
                &format!("play-record/song/list count={}", list.len()),
            );
            if !list.is_empty() {
                return Ok(list);
            }
        }
        Err(error) => crate::logger::log(
            "debug",
            "recent",
            &format!("play-record/song/list failed: {error}"),
        ),
    }

    let uid = state
        .account()
        .ok_or_else(|| "请先登录网易云".to_string())?
        .user_id;
    let value = get_json(
        state,
        "/api/v1/play/record",
        &[
            ("uid", uid),
            ("type", "0".to_string()),
            ("limit", "100".to_string()),
        ],
    )
    .await?;
    let list = parse_recent(&value);
    crate::logger::log(
        "debug",
        "recent",
        &format!("user/record allData count={}", list.len()),
    );
    Ok(list)
}

fn parse_radar_playlists(value: &Value) -> Vec<PlaylistDto> {
    let Some(blocks) = value.pointer("/data/blocks").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for block in blocks {
        let is_radar = block
            .get("blockCode")
            .and_then(Value::as_str)
            .is_some_and(|code| code.contains("MGC_PLAYLIST"));
        if !is_radar {
            continue;
        }
        let Some(creatives) = block.get("creatives").and_then(Value::as_array) else {
            continue;
        };
        for creative in creatives {
            let Some(id) = creative
                .get("creativeId")
                .or_else(|| creative.get("creative_id"))
                .and_then(value_string)
            else {
                continue;
            };
            let ui = creative
                .get("uiElement")
                .or_else(|| creative.get("ui_element"));
            let name = ui
                .and_then(|ui| ui.pointer("/mainTitle/title"))
                .and_then(Value::as_str)
                .or_else(|| creative.get("title").and_then(Value::as_str))
                .unwrap_or("")
                .trim()
                .to_string();
            if name.is_empty() || !seen.insert(id.clone()) {
                continue;
            }
            let cover_url = ui
                .and_then(|ui| ui.pointer("/image/imageUrl"))
                .and_then(Value::as_str)
                .or_else(|| creative.get("imageUrl").and_then(Value::as_str))
                .map(sized_cover)
                .unwrap_or_default();
            result.push(PlaylistDto {
                id,
                name,
                cover_url,
                track_count: 0,
                play_count: 0,
                description: String::new(),
                creator: "网易云音乐".to_string(),
                special_type: 0,
                update_time: 0,
                subscribed: false,
                tracks: Vec::new(),
            });
        }
    }
    result
}

/// Radar cards live in the mobile homepage block stream. Failure is
/// non-fatal: the generic personalized playlists still populate the home page.
pub async fn radar_playlists(state: &AppState) -> Result<Vec<PlaylistDto>, String> {
    let _ = state;
    let value = tokio::task::spawn_blocking(crate::libnetease::homepage_radar)
        .await
        .map_err(|error| format!("雷达歌单线程失败: {error}"))??;
    let playlists = parse_radar_playlists(&value);
    crate::logger::log(
        "debug",
        "home",
        &format!("radar playlists={}", playlists.len()),
    );
    Ok(playlists)
}

pub async fn home_feed(state: &AppState) -> Result<HomeFeed, String> {
    let recommended_query = [("limit", "12".to_string())];
    let new_songs_query = [("limit", "12".to_string())];
    let (recommended_json, new_songs_json, radar_result) = tokio::join!(
        get_json(state, "/api/personalized/playlist", &recommended_query),
        get_json(state, "/api/personalized/newsong", &new_songs_query),
        tokio::time::timeout(Duration::from_secs(10), radar_playlists(state)),
    );
    if recommended_json.is_err() && new_songs_json.is_err() {
        return Err(recommended_json
            .err()
            .or_else(|| new_songs_json.err())
            .unwrap_or_else(|| "首页加载失败".to_string()));
    }

    let mut errors = Vec::new();
    let radar = match radar_result {
        Ok(Ok(playlists)) => playlists,
        Ok(Err(error)) => {
            crate::logger::log("warn", "home", &format!("radar failed: {error}"));
            Vec::new()
        }
        Err(_) => {
            crate::logger::log("warn", "home", "radar timed out");
            Vec::new()
        }
    };
    let recommended = match recommended_json {
        Ok(value) => playlists_from(value.get("result")),
        Err(error) => {
            errors.push(format!("推荐歌单：{error}"));
            Vec::new()
        }
    };
    let new_songs = match new_songs_json {
        Ok(value) => value
            .get("result")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| item.get("song").unwrap_or(item))
                    .filter_map(normalize_song)
                    .collect()
            })
            .unwrap_or_default(),
        Err(error) => {
            errors.push(format!("新歌：{error}"));
            Vec::new()
        }
    };

    let mut daily = Vec::new();
    let mut recent = Vec::new();

    if state.account().is_some() {
        let daily_query = [("limit", "30".to_string())];
        let (daily_result, recent_result) = tokio::join!(
            get_json(state, "/api/v3/discovery/recommend/songs", &daily_query,),
            recent_played(state),
        );

        match daily_result {
            Ok(value) => daily = parse_daily(&value),
            Err(error) => errors.push(format!("每日推荐：{error}")),
        }
        match recent_result {
            Ok(list) => recent = list,
            Err(error) => errors.push(format!("最近播放：{error}")),
        }
    }

    Ok(HomeFeed {
        recommended,
        radar,
        new_songs,
        daily,
        recent,
        error: if errors.is_empty() {
            None
        } else {
            Some(errors.join("；"))
        },
    })
}

async fn fetch_playlist_detail(state: &AppState, id: &str) -> Result<PlaylistDto, String> {
    let value = get_json(
        state,
        "/api/v6/playlist/detail",
        &[
            ("id", id.to_string()),
            ("n", "100000".to_string()),
            ("s", "8".to_string()),
        ],
    )
    .await?;
    value
        .get("playlist")
        .and_then(normalize_playlist)
        .ok_or_else(|| "歌单不存在、已下架或不可访问".to_string())
}

pub async fn playlist_detail(state: &AppState, id: String) -> Result<PlaylistDto, String> {
    fetch_playlist_detail(&state, &id).await
}

pub async fn artist_page(state: &AppState, id: String) -> Result<ArtistPageData, String> {
    let id = id.trim().to_string();
    if id.is_empty() || !id.chars().all(|character| character.is_ascii_digit()) {
        return Err("歌手 ID 无效".to_string());
    }
    let detail = get_json(state, &format!("/api/v1/artist/{id}"), &[]).await?;
    let artist_value = detail
        .get("artist")
        .ok_or_else(|| "网易云没有返回歌手信息".to_string())?;
    let artist = normalize_artist(artist_value).ok_or_else(|| "歌手信息格式异常".to_string())?;
    let songs = normalize_tracks(detail.get("hotSongs"));

    let albums_query = [("limit", "1000".to_string()), ("offset", "0".to_string())];
    let albums_result = get_json(state, &format!("/api/artist/albums/{id}"), &albums_query).await;
    let (albums, album_error) = match albums_result {
        Ok(value) => {
            let albums = value
                .get("hotAlbums")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|album| normalize_album(album, &id, &artist.name))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            (albums, None)
        }
        Err(error) => {
            crate::logger::log("warn", "artist", &format!("id={id} albums failed: {error}"));
            (Vec::new(), Some(error))
        }
    };

    crate::logger::log(
        "info",
        "artist",
        &format!(
            "id={id} name={} songs={} albums={} album_error={}",
            artist.name,
            songs.len(),
            albums.len(),
            album_error.is_some()
        ),
    );
    Ok(ArtistPageData {
        artist,
        songs,
        albums,
        album_error,
    })
}

pub async fn album_page(id: String) -> Result<AlbumPageData, String> {
    let id = id.trim().to_string();
    if id.is_empty() || !id.chars().all(|character| character.is_ascii_digit()) {
        return Err("专辑 ID 无效".to_string());
    }
    let detail_id = id.clone();
    let value = tokio::task::spawn_blocking(move || crate::libnetease::album_detail(&detail_id))
        .await
        .map_err(|e| format!("专辑详情线程失败: {e}"))??;
    let album_value = value
        .get("album")
        .ok_or_else(|| "网易云没有返回专辑信息".to_string())?;
    let artists = album_value
        .get("artists")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|artist| {
                    let id = artist.get("id").and_then(value_string)?;
                    let name = artist
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("未知歌手")
                        .to_string();
                    Some(ArtistRef { id, name })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let artist_id = artists
        .first()
        .map(|artist| artist.id.clone())
        .unwrap_or_default();
    let artist_name = artists
        .iter()
        .map(|artist| artist.name.as_str())
        .collect::<Vec<_>>()
        .join(" / ");
    let album = normalize_album(album_value, &artist_id, &artist_name)
        .ok_or_else(|| "专辑信息格式异常".to_string())?;
    let songs = normalize_tracks(value.get("songs"));
    let description = album_value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    crate::logger::log(
        "info",
        "album",
        &format!("id={id} name={} songs={}", album.name, songs.len()),
    );
    Ok(AlbumPageData {
        album,
        description,
        artists,
        songs,
    })
}

pub async fn library_data(state: &AppState) -> Result<LibraryData, String> {
    let account = state
        .account()
        .ok_or_else(|| "请先登录网易云".to_string())?;
    let uid = account.user_id;

    let playlists_query = [
        ("uid", uid.clone()),
        ("limit", "100".to_string()),
        ("offset", "0".to_string()),
    ];
    let (playlists_result, recent_result) = tokio::join!(
        get_json(state, "/api/user/playlist", &playlists_query),
        recent_played(state),
    );

    let mut errors = Vec::new();
    let playlists = match playlists_result {
        Ok(value) => playlists_from(value.get("playlist")),
        Err(error) => {
            errors.push(format!("歌单：{error}"));
            Vec::new()
        }
    };
    let recent = match recent_result {
        Ok(list) => list,
        Err(error) => {
            errors.push(format!("最近播放：{error}"));
            Vec::new()
        }
    };

    let liked_summary = playlists
        .iter()
        .find(|playlist| playlist.special_type == 5)
        .or_else(|| {
            playlists
                .iter()
                .find(|playlist| playlist.name.contains("喜欢的音乐"))
        })
        .cloned();

    let liked = match liked_summary {
        Some(summary) => match fetch_playlist_detail(&state, &summary.id).await {
            Ok(playlist) => Some(playlist),
            Err(error) => {
                errors.push(format!("我喜欢的音乐：{error}"));
                None
            }
        },
        None => None,
    };

    Ok(LibraryData {
        playlists,
        recent,
        liked,
        error: if errors.is_empty() {
            None
        } else {
            Some(errors.join("；"))
        },
    })
}

pub async fn create_playlist(name: String) -> Result<String, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("歌单名称不能为空".to_string());
    }
    if name.chars().count() > 40 {
        return Err("歌单名称过长".to_string());
    }
    tokio::task::spawn_blocking(move || crate::libnetease::playlist_create(&name))
        .await
        .map_err(|error| format!("新建歌单线程失败: {error}"))?
}

pub async fn subscribe_playlist(id: String, subscribe: bool) -> Result<(), String> {
    let id = id.trim().to_string();
    if id.is_empty() || !id.chars().all(|character| character.is_ascii_digit()) {
        return Err("歌单 ID 无效".to_string());
    }
    tokio::task::spawn_blocking(move || crate::libnetease::playlist_subscribe(&id, subscribe))
        .await
        .map_err(|error| format!("收藏歌单线程失败: {error}"))?
}

pub async fn search_music(state: &AppState, keyword: String) -> Result<SearchData, String> {
    let keyword = keyword.trim().to_string();
    if keyword.is_empty() {
        return Ok(SearchData::empty());
    }

    let song_query = [
        ("s", keyword.clone()),
        ("type", "1".to_string()),
        ("limit", "30".to_string()),
        ("offset", "0".to_string()),
    ];
    let playlist_query = [
        ("s", keyword.clone()),
        ("type", "1000".to_string()),
        ("limit", "12".to_string()),
        ("offset", "0".to_string()),
    ];
    let artist_query = [
        ("s", keyword.clone()),
        ("type", "100".to_string()),
        ("limit", "18".to_string()),
        ("offset", "0".to_string()),
    ];
    let album_query = [
        ("s", keyword),
        ("type", "10".to_string()),
        ("limit", "24".to_string()),
        ("offset", "0".to_string()),
    ];
    let (songs_result, playlists_result, artists_result, albums_result) = tokio::join!(
        get_json(state, "/api/cloudsearch/pc", &song_query),
        get_json(state, "/api/cloudsearch/pc", &playlist_query),
        get_json(state, "/api/cloudsearch/pc", &artist_query),
        get_json(state, "/api/cloudsearch/pc", &album_query),
    );

    let mut errors = Vec::new();
    if let Err(error) = &songs_result {
        errors.push(format!("歌曲：{error}"));
    }
    if let Err(error) = &playlists_result {
        errors.push(format!("歌单：{error}"));
    }
    if let Err(error) = &artists_result {
        errors.push(format!("歌手：{error}"));
    }
    if let Err(error) = &albums_result {
        errors.push(format!("专辑：{error}"));
    }
    if songs_result.is_err()
        && playlists_result.is_err()
        && artists_result.is_err()
        && albums_result.is_err()
    {
        return Err(songs_result
            .err()
            .or_else(|| playlists_result.err())
            .or_else(|| artists_result.err())
            .or_else(|| albums_result.err())
            .unwrap_or_else(|| "搜索失败，请稍后再试".to_string()));
    }

    let songs = songs_result
        .ok()
        .and_then(|value| {
            value
                .pointer("/result/songs")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(normalize_song).collect())
        })
        .unwrap_or_default();
    let playlists = playlists_result
        .ok()
        .and_then(|value| {
            value
                .pointer("/result/playlists")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(normalize_playlist).collect())
        })
        .unwrap_or_default();

    let artists = artists_result
        .ok()
        .and_then(|value| {
            value
                .pointer("/result/artists")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(normalize_artist).collect())
        })
        .unwrap_or_default();
    let albums = albums_result
        .ok()
        .and_then(|value| {
            value
                .pointer("/result/albums")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|album| normalize_album(album, "", ""))
                        .collect()
                })
        })
        .unwrap_or_default();

    Ok(SearchData {
        songs,
        playlists,
        artists,
        albums,
        error: if errors.is_empty() {
            None
        } else {
            Some(errors.join("；"))
        },
        ..SearchData::empty()
    })
}

pub async fn search_kind(
    state: &AppState,
    kind: String,
    keyword: String,
) -> Result<SearchData, String> {
    let keyword = keyword.trim().to_string();
    if keyword.is_empty() {
        return Ok(SearchData::empty());
    }
    let (type_id, limit) = match kind.as_str() {
        "playlist" => ("1000", "24"),
        "artist" => ("100", "18"),
        "album" => ("10", "24"),
        "lyric" => ("1006", "18"),
        _ => ("1", "30"),
    };
    let query = [
        ("s", keyword.clone()),
        ("type", type_id.to_string()),
        ("limit", limit.to_string()),
        ("offset", "0".to_string()),
    ];
    let value = get_json(state, "/api/cloudsearch/pc", &query).await?;
    let result = value.get("result");
    let mut data = SearchData::empty();

    match kind.as_str() {
        "playlist" => {
            data.playlists = result
                .and_then(|value| value.get("playlists"))
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(normalize_playlist).collect())
                .unwrap_or_default();
        }
        "artist" => {
            data.artists = result
                .and_then(|value| value.get("artists"))
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(normalize_artist).collect())
                .unwrap_or_default();
        }
        "album" => {
            data.albums = result
                .and_then(|value| value.get("albums"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|album| normalize_album(album, "", ""))
                        .collect()
                })
                .unwrap_or_default();
        }
        "lyric" => {
            let needle = keyword.to_lowercase();
            data.lyrics = result
                .and_then(|value| value.get("songs"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|song| {
                            let track = normalize_song(song)?;
                            let all = song
                                .get("lyrics")
                                .and_then(Value::as_array)
                                .map(|lines| {
                                    lines
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .map(|line| {
                                            line.replace("<b>", "")
                                                .replace("</b>", "")
                                                .replace("<em>", "")
                                                .replace("</em>", "")
                                        })
                                        .filter(|line| !line.trim().is_empty())
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            let matched = all
                                .iter()
                                .filter(|line| line.to_lowercase().contains(&needle))
                                .take(3)
                                .cloned()
                                .collect::<Vec<_>>();
                            let lines = if matched.is_empty() {
                                all.into_iter().take(4).collect()
                            } else {
                                matched
                            };
                            Some(LyricSearchDto { track, lines })
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        _ => {
            data.songs = result
                .and_then(|value| value.get("songs"))
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(normalize_song).collect())
                .unwrap_or_default();
        }
    }

    crate::logger::log(
        "debug",
        "search",
        &format!(
            "kind={kind} keyword_len={} songs={} playlists={} artists={} albums={} lyrics={}",
            keyword.len(),
            data.songs.len(),
            data.playlists.len(),
            data.artists.len(),
            data.albums.len(),
            data.lyrics.len()
        ),
    );
    Ok(data)
}

fn parse_lrc(text: &str) -> Vec<(f64, String)> {
    let mut lines = Vec::new();
    let mut offset_ms = 0.0f64;
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            let tag = &rest[1..end];
            if let Some(value) = tag
                .strip_prefix("offset:")
                .and_then(|value| value.trim().parse::<f64>().ok())
            {
                offset_ms = value;
            } else if let Some(time) = parse_lrc_time(tag) {
                times.push(time);
            }
            rest = &rest[end + 1..];
        }
        let content = rest.trim();
        if content.is_empty() {
            continue;
        }
        for time in times {
            lines.push((time, content.to_string()));
        }
    }
    lines.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let offset = offset_ms / 1000.0;
    if offset != 0.0 {
        for line in &mut lines {
            line.0 = (line.0 + offset).max(0.0);
        }
    }
    lines
}

fn parse_lrc_time(tag: &str) -> Option<f64> {
    let (minutes, seconds) = tag.split_once(':')?;
    let minutes: f64 = minutes.trim().parse().ok()?;
    let seconds: f64 = seconds.trim().parse().ok()?;
    Some(minutes * 60.0 + seconds)
}

pub async fn lyric(state: &AppState, id: String) -> Result<LyricData, String> {
    let value = get_json(
        &state,
        "/api/song/lyric",
        &[
            ("id", id),
            ("lv", "-1".to_string()),
            ("kv", "-1".to_string()),
            ("tv", "-1".to_string()),
        ],
    )
    .await?;

    let original = value
        .pointer("/lrc/lyric")
        .and_then(Value::as_str)
        .unwrap_or("");
    let translated = value
        .pointer("/tlyric/lyric")
        .and_then(Value::as_str)
        .unwrap_or("");

    let mut translations: BTreeMap<i64, String> = BTreeMap::new();
    for (time, text) in parse_lrc(translated) {
        translations.insert((time * 1000.0).round() as i64, text);
    }

    let lines = parse_lrc(original)
        .into_iter()
        .map(|(time, text)| {
            let key = (time * 1000.0).round() as i64;
            LyricLine {
                time,
                text,
                translation: translations.get(&key).cloned(),
            }
        })
        .collect();

    Ok(LyricData { lines })
}

pub async fn like_song(state: &AppState, id: String, like: bool) -> Result<LikeResult, String> {
    if state.account().is_none() {
        return Err("请先登录网易云".to_string());
    }
    let id_for_log = id.clone();
    tokio::task::spawn_blocking(move || crate::libnetease::song_like(&id, like))
        .await
        .map_err(|e| format!("喜欢操作线程失败: {e}"))??;
    crate::logger::log("info", "like", &format!("id={id_for_log} like={like}"));
    Ok(LikeResult { liked: like })
}

pub async fn report_play_start(
    state: &AppState,
    id: String,
    source_id: String,
) -> Result<(), String> {
    if state.account().is_none() {
        return Err("请先登录网易云".to_string());
    }
    tokio::task::spawn_blocking(move || crate::libnetease::report_play_start(&id, &source_id))
        .await
        .map_err(|error| format!("播放上报线程失败: {error}"))?
}

#[derive(Clone, Debug)]
pub struct ResolvedSong {
    pub url: String,
    pub level: String,
    pub trial: bool,
}

/// Resolve a playable CDN URL through libnetease's weapi/eapi implementation.
/// The original Cirro library adds the `os=ios`/`appver` cookie pair that the
/// player URL endpoint requires; a plain web request gets `url: null`.
pub async fn resolve_song_url(
    state: &AppState,
    id: &str,
    requested_level: Option<&str>,
) -> Result<ResolvedSong, String> {
    if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_digit()) {
        return Err("无效的歌曲 ID".to_string());
    }
    let requested_level = requested_level.filter(|level| {
        matches!(
            *level,
            "auto" | "standard" | "higher" | "exhigh" | "lossless" | "hires"
        )
    });
    let cache_key = format!("{}:{id}", requested_level.unwrap_or("auto"));

    if let Ok(cache) = state.url_cache.lock() {
        if let Some((url, level, trial, at)) = cache.get(&cache_key) {
            if at.elapsed() < Duration::from_secs(240) {
                return Ok(ResolvedSong {
                    url: url.clone(),
                    level: level.clone(),
                    trial: *trial,
                });
            }
        }
    }

    let id_owned = id.to_string();
    let requested = requested_level.map(str::to_string);
    let resolved = tokio::task::spawn_blocking(move || {
        let mut levels = Vec::new();
        if let Some(level) = requested {
            levels.push(level);
        }
        for fallback in ["exhigh", "standard"] {
            if !levels.iter().any(|level| level == fallback) {
                levels.push(fallback.to_string());
            }
        }
        let mut last_error = String::new();
        for level in levels.iter().map(String::as_str) {
            match crate::libnetease::song_url(&id_owned, level) {
                Ok((url, trial)) => return Ok((url, level.to_string(), trial)),
                Err(error) => {
                    crate::logger::log(
                        "debug",
                        "song",
                        &format!("id={id_owned} level={level} failed: {error}"),
                    );
                    last_error = error;
                }
            }
        }
        Err(if last_error.is_empty() {
            "该歌曲暂不可播放".to_string()
        } else {
            last_error
        })
    })
    .await
    .map_err(|e| format!("音源解析线程失败: {e}"))??;

    let (url, level, trial) = resolved;
    let url = url
        .strip_prefix("http://")
        .map(|rest| format!("https://{rest}"))
        .unwrap_or_else(|| url.to_string());
    let parsed = reqwest::Url::parse(&url).map_err(|_| "音源地址格式异常".to_string())?;
    if parsed.scheme() != "https" {
        return Err("音源地址必须使用 HTTPS".to_string());
    }
    if !parsed
        .host_str()
        .is_some_and(crate::state::is_allowed_audio_host)
    {
        return Err("音源地址不在允许的网易云域名内".to_string());
    }

    crate::logger::log(
        "debug",
        "song",
        &format!(
            "id={} url_host={} level={level} trial={trial}",
            id,
            reqwest::Url::parse(&url)
                .ok()
                .and_then(|parsed| parsed.host_str().map(str::to_string))
                .unwrap_or_else(|| "?".to_string())
        ),
    );
    if let Ok(mut cache) = state.url_cache.lock() {
        cache.insert(
            cache_key,
            (url.clone(), level.clone(), trial, Instant::now()),
        );
    }
    Ok(ResolvedSong { url, level, trial })
}

#[cfg(test)]
mod tests {
    use super::parse_lrc;

    #[test]
    fn lrc_handles_multiple_timestamps() {
        let lines = parse_lrc("[00:01.00][00:02.50]hello\n[00:03.00]world");
        assert_eq!(lines.len(), 3);
        assert!((lines[0].0 - 1.0).abs() < 0.001);
        assert!((lines[1].0 - 2.5).abs() < 0.001);
        assert_eq!(lines[2].1, "world");
    }

    #[test]
    fn lrc_applies_offset() {
        let lines = parse_lrc("[offset:500]\n[00:01.00]hello");
        assert_eq!(lines.len(), 1);
        assert!((lines[0].0 - 1.5).abs() < 0.001);
    }
}
