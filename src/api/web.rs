//! Static SPA serving. The frontend build is served from `TAR_STATIC_DIR` (default
//! `frontend/dist`); unknown paths fall through to `index.html` so client-side routing works.

use crate::state::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

pub async fn spa(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if let Some(dir) = &state.config.static_dir {
        if !path.is_empty() {
            let candidate = std::path::Path::new(dir).join(path);
            if candidate.is_file() && candidate.starts_with(dir) {
                if let Ok(bytes) = tokio::fs::read(&candidate).await {
                    let ct = content_type(&candidate);
                    return ([(header::CONTENT_TYPE, ct)], bytes).into_response();
                }
            }
        }
    }
    // An API path that reached the fallback is a 404, not an HTML page.
    if path.starts_with("api/") {
        return crate::error::AppError::not_found(format!("no route for /{path}")).into_response();
    }
    spa_response(&state).await
}

pub async fn spa_response(state: &AppState) -> Response {
    if let Some(dir) = &state.config.static_dir {
        let index = std::path::Path::new(dir).join("index.html");
        if let Ok(html) = tokio::fs::read(&index).await {
            return Response::builder()
                .header(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"))
                .body(Body::from(html))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    }
    // No UI build present: say so usefully rather than 404ing a browser.
    let body = format!(
        r#"<!doctype html><meta charset="utf-8"><title>{title}</title>
<style>body{{font:15px/1.6 system-ui,sans-serif;max-width:44rem;margin:4rem auto;padding:0 1.5rem;color:#1a1a1a}}
code{{background:#f2f2f2;padding:.15em .4em;border-radius:3px}}a{{color:#0b5cad}}</style>
<h1>{title}</h1>
<p>The API is running. The web UI is not built into this deployment.</p>
<p>Build it with <code>cd frontend &amp;&amp; npm install &amp;&amp; npm run build</code>, or point
<code>TAR_STATIC_DIR</code> at an existing build.</p>
<ul>
<li><a href="/.well-known/tar-registry">/.well-known/tar-registry</a> — registry self-description</li>
<li><a href="/api/v1/registry">/api/v1/registry</a> — catalogue record</li>
<li><a href="/api/v1/software">/api/v1/software</a> — registered software</li>
<li><a href="/healthz">/healthz</a></li>
</ul>"#,
        title = html_escape(&state.config.title)
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"))
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn content_type(p: &std::path::Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("woff2") => "font/woff2",
        Some("ico") => "image/x-icon",
        Some("ttl") => "text/turtle",
        _ => "application/octet-stream",
    }
}
