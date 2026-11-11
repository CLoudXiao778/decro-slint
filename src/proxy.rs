use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::collections::HashMap;

use crate::netease;
use crate::state::{is_allowed_audio_host, AppState};

const FORWARDED_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "content-range",
    "accept-ranges",
    "etag",
    "last-modified",
    "cache-control",
];

pub async fn serve(listener: tokio::net::TcpListener, state: AppState) -> Result<(), String> {
    let app = Router::new()
        .route("/stream/{id}", get(stream).options(options))
        .with_state(state);
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("音频代理服务退出: {e}"))
}

async fn options() -> Response {
    (
        StatusCode::NO_CONTENT,
        [
            ("access-control-allow-origin", "*"),
            ("access-control-allow-methods", "GET, HEAD, OPTIONS"),
            ("access-control-allow-headers", "Range"),
            ("access-control-max-age", "86400"),
        ],
    )
        .into_response()
}

async fn stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if params.get("token").map(String::as_str) != Some(state.proxy.token.as_str()) {
        crate::logger::log("warn", "proxy", &format!("id={id} forbidden token"));
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }

    let range = headers
        .get(axum::http::header::RANGE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    crate::logger::log("debug", "proxy", &format!("id={id} range={range}"));

    let level = params.get("level").map(String::as_str);
    let upstream_url = match netease::resolve_song_url(&state, &id, level).await {
        Ok(url) => url,
        Err(error) => {
            crate::logger::log("warn", "proxy", &format!("id={id} resolve: {error}"));
            return (
                StatusCode::BAD_GATEWAY,
                [("content-type", "text/plain; charset=utf-8")],
                error,
            )
                .into_response();
        }
    };

    let mut request = state
        .stream
        .get(&upstream_url)
        .header(reqwest::header::REFERER, "https://music.163.com/")
        .header(reqwest::header::ACCEPT, "*/*");
    if let Some(range) = headers.get(axum::http::header::RANGE) {
        if let Ok(range) = range.to_str() {
            request = request.header(reqwest::header::RANGE, range);
        }
    }

    let upstream = match request.send().await {
        Ok(response) => response,
        Err(_) => {
            crate::logger::log(
                "error",
                "proxy",
                &format!("upstream request failed id={id}"),
            );
            return (
                StatusCode::BAD_GATEWAY,
                [("content-type", "text/plain; charset=utf-8")],
                "音源连接失败",
            )
                .into_response();
        }
    };

    if !upstream.url().host_str().is_some_and(is_allowed_audio_host) {
        return (
            StatusCode::BAD_GATEWAY,
            [("content-type", "text/plain; charset=utf-8")],
            "音源域名异常",
        )
            .into_response();
    }

    let status = upstream.status();
    crate::logger::log(
        "debug",
        "proxy",
        &format!(
            "id={id} upstream_status={} content_length={}",
            status,
            upstream
                .headers()
                .get("content-length")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("-")
        ),
    );
    let mut builder = Response::builder()
        .status(status.as_u16())
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-headers", "Range")
        .header(
            "access-control-expose-headers",
            "Content-Length, Content-Range",
        );
    for name in FORWARDED_HEADERS {
        if let Some(value) = upstream.headers().get(*name) {
            builder = builder.header(*name, value);
        }
    }

    match builder.body(Body::from_stream(upstream.bytes_stream())) {
        Ok(response) => response,
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("音频响应构建失败: {error}"),
        )
            .into_response(),
    }
}
