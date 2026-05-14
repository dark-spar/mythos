//! HTTP byte-range streaming for media files.
//!
//! Shared between `/api/movies/:id/stream` and `/api/episodes/:id/stream`.
//! Movie and episode handlers each resolve their path param to a
//! `file_id` and then delegate to [`stream_file`].

use std::io::SeekFrom;
use std::path::PathBuf;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

/// Serve a media file by `file_id` with HTTP byte-range support so
/// `<video>` can seek without re-downloading and the browser can spool
/// playback before the whole file arrives.
pub async fn stream_file(
    pool: &SqlitePool,
    file_id: Uuid,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT l.root_path, f.path \
         FROM media_files f JOIN libraries l ON l.id = f.library_id \
         WHERE f.id = ?",
    )
    .bind(file_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "stream lookup failed");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;

    let (root, rel) = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;
    let abs = PathBuf::from(root).join(rel);

    let metadata = tokio::fs::metadata(&abs).await.map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ApiError::new(StatusCode::NOT_FOUND, "file_missing")
        } else {
            tracing::error!(?err, path = %abs.display(), "stat failed");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    })?;
    if !metadata.is_file() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "file_missing"));
    }
    let size = metadata.len();
    let mime = mime_guess::from_path(&abs).first_or_octet_stream();
    let mime_header = HeaderValue::from_str(mime.as_ref())
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));

    let range = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(value) => match parse_range(value, size) {
            Ok(maybe) => maybe,
            Err(()) => {
                let mut res = (StatusCode::RANGE_NOT_SATISFIABLE, Body::empty()).into_response();
                res.headers_mut().insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{size}"))
                        .expect("size formats cleanly"),
                );
                return Ok(res);
            }
        },
        None => None,
    };

    let file = tokio::fs::File::open(&abs).await.map_err(|err| {
        tracing::error!(?err, path = %abs.display(), "open failed");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;

    if let Some((start, end)) = range {
        let mut file = file;
        if start > 0 {
            file.seek(SeekFrom::Start(start)).await.map_err(|err| {
                tracing::error!(?err, "seek failed");
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
            })?;
        }
        let take_len = end - start + 1;
        let reader = file.take(take_len);
        let body = Body::from_stream(ReaderStream::new(reader));
        let mut res = (StatusCode::PARTIAL_CONTENT, body).into_response();
        let h = res.headers_mut();
        h.insert(header::CONTENT_TYPE, mime_header);
        h.insert(header::CONTENT_LENGTH, header_num(take_len));
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{size}"))
                .expect("range formats cleanly"),
        );
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        Ok(res)
    } else {
        let body = Body::from_stream(ReaderStream::new(file));
        let mut res = (StatusCode::OK, body).into_response();
        let h = res.headers_mut();
        h.insert(header::CONTENT_TYPE, mime_header);
        h.insert(header::CONTENT_LENGTH, header_num(size));
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        Ok(res)
    }
}

/// Parse an RFC 7233 single-range `Range: bytes=…` header.
///
/// Returns:
/// - `Ok(Some((start, end)))` on success
/// - `Ok(None)` if the header isn't a `bytes=` range (treat as no range)
/// - `Err(())` if the range is syntactically present but unsatisfiable
///   (caller responds 416)
///
/// Multi-range (`bytes=0-100,200-300`) is intentionally rejected — the
/// HTML5 video element only ever asks for single ranges, so the
/// implementation cost isn't justified.
fn parse_range(value: &str, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') {
        return Err(());
    }
    let mut parts = spec.splitn(2, '-');
    let start_s = parts.next().ok_or(())?.trim();
    let end_s = parts.next().ok_or(())?.trim();

    let (start, end) = if start_s.is_empty() {
        // bytes=-N → last N bytes
        let n: u64 = end_s.parse().map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        let start = size.saturating_sub(n);
        (start, size - 1)
    } else if end_s.is_empty() {
        // bytes=N- → from N to end
        let start: u64 = start_s.parse().map_err(|_| ())?;
        if start >= size {
            return Err(());
        }
        (start, size - 1)
    } else {
        let start: u64 = start_s.parse().map_err(|_| ())?;
        let end: u64 = end_s.parse().map_err(|_| ())?;
        if start > end || start >= size {
            return Err(());
        }
        (start, end.min(size - 1))
    };
    Ok(Some((start, end)))
}

fn header_num(n: u64) -> HeaderValue {
    HeaderValue::from_str(&n.to_string()).expect("u64 formats as ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_full() {
        assert_eq!(parse_range("bytes=0-99", 1000), Ok(Some((0, 99))));
    }

    #[test]
    fn range_open_ended() {
        assert_eq!(parse_range("bytes=500-", 1000), Ok(Some((500, 999))));
    }

    #[test]
    fn range_suffix() {
        assert_eq!(parse_range("bytes=-100", 1000), Ok(Some((900, 999))));
    }

    #[test]
    fn range_clamps_end_to_size() {
        assert_eq!(parse_range("bytes=0-9999", 1000), Ok(Some((0, 999))));
    }

    #[test]
    fn range_invalid_start_past_end() {
        assert_eq!(parse_range("bytes=2000-", 1000), Err(()));
    }

    #[test]
    fn range_invalid_inverted() {
        assert_eq!(parse_range("bytes=100-50", 1000), Err(()));
    }

    #[test]
    fn range_no_bytes_prefix_is_ignored() {
        assert_eq!(parse_range("seconds=0-10", 1000), Ok(None));
    }

    #[test]
    fn range_multi_rejected() {
        assert_eq!(parse_range("bytes=0-99,200000-200099", 1000), Err(()));
    }
}
