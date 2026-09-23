use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::header::{
    ACCEPT_RANGES, ACCESS_CONTROL_ALLOW_ORIGIN, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, ReadBuf, SeekFrom};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::io::ReaderStream;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone)]
struct MediaState {
    file: PathBuf,
    token: String,
    size: u64,
    mime: String,
    stats: TransferStats,
}

#[derive(Debug)]
struct TransferStatsInner {
    bytes_sent: AtomicU64,
    first_byte_at: OnceLock<Instant>,
    file_size: u64,
}

#[derive(Debug, Clone)]
pub struct TransferStats {
    inner: Arc<TransferStatsInner>,
}

#[derive(Debug, Clone, Copy)]
pub struct TransferSnapshot {
    pub bytes_sent: u64,
    pub file_size: u64,
    pub elapsed: Duration,
    pub average_mbps: f64,
}

impl TransferStats {
    fn new(file_size: u64) -> Self {
        Self {
            inner: Arc::new(TransferStatsInner {
                bytes_sent: AtomicU64::new(0),
                first_byte_at: OnceLock::new(),
                file_size,
            }),
        }
    }

    fn add_bytes(&self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.inner.first_byte_at.get_or_init(Instant::now);
        self.inner.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> TransferSnapshot {
        let bytes_sent = self.inner.bytes_sent.load(Ordering::Relaxed);
        let elapsed = self
            .inner
            .first_byte_at
            .get()
            .map(Instant::elapsed)
            .unwrap_or_default();
        let average_mbps = if elapsed.is_zero() {
            0.0
        } else {
            bytes_sent as f64 * 8.0 / elapsed.as_secs_f64() / 1_000_000.0
        };
        TransferSnapshot {
            bytes_sent,
            file_size: self.inner.file_size,
            elapsed,
            average_mbps,
        }
    }
}

struct CountingReader<R> {
    inner: R,
    stats: TransferStats,
}

impl<R: AsyncRead + Unpin> AsyncRead for CountingReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(context, buffer);
        if matches!(&result, Poll::Ready(Ok(()))) {
            self.stats
                .add_bytes((buffer.filled().len() - before) as u64);
        }
        result
    }
}

pub struct MediaServer {
    address: SocketAddr,
    token: String,
    file_name: String,
    mime: String,
    stats: TransferStats,
    task: JoinHandle<()>,
}

impl MediaServer {
    pub async fn start(file: &Path, port: u16) -> Result<Self> {
        let canonical = tokio::fs::canonicalize(file)
            .await
            .with_context(|| format!("media file does not exist: {}", file.display()))?;
        let metadata = tokio::fs::metadata(&canonical)
            .await
            .context("failed to read media file metadata")?;
        if !metadata.is_file() {
            bail!("media source is not a regular file: {}", file.display());
        }

        let file_name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("media")
            .to_owned();
        let mime = mime_guess::from_path(&canonical)
            .first_or_octet_stream()
            .essence_str()
            .to_owned();
        let token = Uuid::new_v4().simple().to_string();
        let stats = TransferStats::new(metadata.len());
        let state = MediaState {
            file: canonical,
            token: token.clone(),
            size: metadata.len(),
            mime: mime.clone(),
            stats: stats.clone(),
        };

        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
            .await
            .with_context(|| format!("failed to bind media server on port {port}"))?;
        let address = listener.local_addr()?;
        let app = Router::new()
            .route("/media/{token}/{name}", get(serve_media))
            .with_state(state);
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                eprintln!("media server stopped unexpectedly: {error}");
            }
        });

        Ok(Self {
            address,
            token,
            file_name,
            mime,
            stats,
            task,
        })
    }

    pub fn url_for_device(&self, device_location: &Url) -> Result<Url> {
        let local_ip = route_local_ip(device_location)?;
        let encoded_name: String =
            url::form_urlencoded::byte_serialize(self.file_name.as_bytes()).collect();
        Url::parse(&format!(
            "http://{local_ip}:{}/media/{}/{encoded_name}",
            self.address.port(),
            self.token
        ))
        .context("failed to construct the local media URL")
    }

    pub fn mime(&self) -> &str {
        &self.mime
    }

    pub fn stats(&self) -> TransferStats {
        self.stats.clone()
    }
}

impl Drop for MediaServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_media(
    State(state): State<MediaState>,
    AxumPath((token, _name)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response<Body> {
    if token != state.token {
        return response(StatusCode::NOT_FOUND, Body::empty(), &state, None);
    }

    let range = match headers.get("range").and_then(|value| value.to_str().ok()) {
        Some(value) => match parse_range(value, state.size) {
            Some(range) => Some(range),
            None => {
                let mut result = response(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    Body::empty(),
                    &state,
                    None,
                );
                result.headers_mut().insert(
                    CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{}", state.size)).unwrap(),
                );
                return result;
            }
        },
        None => None,
    };

    let mut file = match File::open(&state.file).await {
        Ok(file) => file,
        Err(_) => {
            return response(
                StatusCode::INTERNAL_SERVER_ERROR,
                Body::empty(),
                &state,
                None,
            );
        }
    };
    let (status, start, end) = match range {
        Some((start, end)) => (StatusCode::PARTIAL_CONTENT, start, end),
        None => (StatusCode::OK, 0, state.size.saturating_sub(1)),
    };
    if file.seek(SeekFrom::Start(start)).await.is_err() {
        return response(
            StatusCode::INTERNAL_SERVER_ERROR,
            Body::empty(),
            &state,
            None,
        );
    }

    let length = if state.size == 0 { 0 } else { end - start + 1 };
    let reader = CountingReader {
        inner: file.take(length),
        stats: state.stats.clone(),
    };
    let stream = ReaderStream::new(reader);
    let mut result = response(status, Body::from_stream(stream), &state, Some(length));
    if status == StatusCode::PARTIAL_CONTENT {
        result.headers_mut().insert(
            CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{}", state.size)).unwrap(),
        );
    }
    result
}

fn response(
    status: StatusCode,
    body: Body,
    state: &MediaState,
    length: Option<u64>,
) -> Response<Body> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    if let Ok(value) = HeaderValue::from_str(&state.mime) {
        headers.insert(CONTENT_TYPE, value);
    }
    if let Some(length) = length
        && let Ok(value) = HeaderValue::from_str(&length.to_string())
    {
        headers.insert(CONTENT_LENGTH, value);
    }
    headers.insert(
        "transfermode.dlna.org",
        HeaderValue::from_static("Streaming"),
    );
    headers.insert(
        "contentfeatures.dlna.org",
        HeaderValue::from_static(
            "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000",
        ),
    );
    response
}

pub fn parse_range(value: &str, size: u64) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let value = value.strip_prefix("bytes=")?;
    if value.contains(',') {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    match (start.trim(), end.trim()) {
        ("", "") => None,
        ("", suffix) => {
            let suffix: u64 = suffix.parse().ok()?;
            if suffix == 0 {
                return None;
            }
            let length = suffix.min(size);
            Some((size - length, size - 1))
        }
        (start, "") => {
            let start: u64 = start.parse().ok()?;
            (start < size).then_some((start, size - 1))
        }
        (start, end) => {
            let start: u64 = start.parse().ok()?;
            let end: u64 = end.parse().ok()?;
            if start >= size || start > end {
                None
            } else {
                Some((start, end.min(size - 1)))
            }
        }
    }
}

fn route_local_ip(device_location: &Url) -> Result<IpAddr> {
    let host = device_location
        .host_str()
        .context("device location has no host")?;
    let port = device_location.port_or_known_default().unwrap_or(80);
    let target = (host, port)
        .to_socket_addrs()
        .context("failed to resolve the TV address")?
        .next()
        .context("the TV address resolved to no IP addresses")?;
    let bind_address = match target {
        SocketAddr::V4(_) => "0.0.0.0:0",
        SocketAddr::V6(_) => "[::]:0",
    };
    let socket = std::net::UdpSocket::bind(bind_address)
        .context("failed to determine the local network address")?;
    socket
        .connect(target)
        .context("there is no network route to the TV")?;
    Ok(socket.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::RANGE;

    #[test]
    fn parses_http_byte_ranges() {
        assert_eq!(parse_range("bytes=0-99", 1_000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1_000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-50", 1_000), Some((950, 999)));
        assert_eq!(parse_range("bytes=900-1200", 1_000), Some((900, 999)));
        assert_eq!(parse_range("bytes=1000-", 1_000), None);
        assert_eq!(parse_range("bytes=20-10", 1_000), None);
        assert_eq!(parse_range("items=0-1", 1_000), None);
        assert_eq!(parse_range("bytes=0-1,4-5", 1_000), None);
    }

    #[tokio::test]
    async fn serves_full_and_partial_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample video.mp4");
        std::fs::write(&path, b"0123456789").unwrap();
        let server = MediaServer::start(&path, 0).await.unwrap();
        let url = server
            .url_for_device(&Url::parse("http://127.0.0.1/device.xml").unwrap())
            .unwrap();
        let client = reqwest::Client::new();

        let full = client.get(url.clone()).send().await.unwrap();
        assert_eq!(full.status(), StatusCode::OK);
        assert_eq!(full.headers()[CONTENT_LENGTH], "10");
        assert_eq!(full.bytes().await.unwrap().as_ref(), b"0123456789");

        let partial = client
            .get(url.clone())
            .header(RANGE, "bytes=2-5")
            .send()
            .await
            .unwrap();
        assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(partial.headers()[CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(partial.bytes().await.unwrap().as_ref(), b"2345");

        let head = client.head(url).send().await.unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(head.headers()[CONTENT_LENGTH], "10");
        assert!(head.bytes().await.unwrap().is_empty());

        let snapshot = server.stats().snapshot();
        assert_eq!(snapshot.bytes_sent, 14);
        assert_eq!(snapshot.file_size, 10);
        assert!(snapshot.average_mbps > 0.0);
    }
}
