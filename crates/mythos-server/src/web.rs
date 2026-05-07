//! Serves the embedded SvelteKit SPA. Asset requests return the bundled file;
//! everything else falls back to `index.html` so client-side routing works.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../web/build"]
struct Spa;

pub async fn handler(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    serve(path).await
}

async fn serve(path: &str) -> Response {
    if let Some(asset) = Spa::get(path) {
        return asset_response(path, asset);
    }
    // SPA fallback: any unknown path serves index.html for client-side routing.
    if let Some(index) = Spa::get("index.html") {
        return asset_response("index.html", index);
    }
    not_found()
}

fn asset_response(path: &str, asset: rust_embed::EmbeddedFile) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mut res = Response::new(Body::from(asset.data.into_owned()));
    res.headers_mut()
        .insert(header::CONTENT_TYPE, header_value(mime.as_ref()));

    // Hashed assets under /_app/immutable/* are content-addressed; cache hard.
    // Everything else (index.html, robots.txt) should revalidate.
    let cache_control = if path.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    res.headers_mut()
        .insert(header::CACHE_CONTROL, header_value(cache_control));
    res
}

fn header_value(s: &str) -> HeaderValue {
    HeaderValue::from_str(s).unwrap_or(HeaderValue::from_static("application/octet-stream"))
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}
