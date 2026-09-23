use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use url::Url;

use crate::device::Device;
use crate::discovery::discover_devices;
use crate::media::{MediaServer, TransferStats};
use crate::soap::{DlnaClient, escape_xml};

#[derive(Clone)]
struct BridgeState {
    token: Arc<str>,
    discovery_timeout: Duration,
    session: Arc<RwLock<Option<BridgeSession>>>,
}

struct BridgeSession {
    client: DlnaClient,
    _media_server: Option<MediaServer>,
    transfer_stats: Option<TransferStats>,
    source: String,
    device_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceView {
    index: usize,
    friendly_name: String,
    manufacturer: String,
    model_name: String,
    address: String,
}

impl DeviceView {
    fn from_device(index: usize, device: &Device) -> Self {
        Self {
            index,
            friendly_name: device.friendly_name.clone(),
            manufacturer: device.manufacturer.clone(),
            model_name: device.model_name.clone(),
            address: device.location.host_str().unwrap_or_default().to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CastRequest {
    #[serde(alias = "url")]
    source: String,
    device: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlRequest {
    action: String,
    value: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackStatus {
    position: String,
    duration: String,
    position_seconds: u64,
    duration_seconds: u64,
    source: String,
    device_name: String,
    bytes_sent: Option<u64>,
    file_size: Option<u64>,
    average_mbps: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiResult {
    ok: bool,
    message: String,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "invalid or missing bridge token".to_owned(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ApiResult {
                ok: false,
                message: self.message,
            }),
        )
            .into_response()
    }
}

pub async fn serve_bridge(
    address: IpAddr,
    port: u16,
    token: String,
    discovery_timeout: Duration,
) -> Result<()> {
    let state = BridgeState {
        token: Arc::from(token),
        discovery_timeout,
        session: Arc::new(RwLock::new(None)),
    };
    let cors = CorsLayer::new()
        .allow_origin(HeaderValue::from_static("*"))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([AUTHORIZATION, CONTENT_TYPE]);
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/devices", get(devices))
        .route("/api/cast", post(cast))
        .route("/api/control", post(control))
        .route("/api/status", get(playback_status))
        .layer(cors)
        .with_state(state);
    let listener = TcpListener::bind((address, port))
        .await
        .with_context(|| format!("failed to bind xscreen bridge on {address}:{port}"))?;
    axum::serve(listener, app)
        .await
        .context("xscreen bridge stopped unexpectedly")?;
    Ok(())
}

async fn health() -> Json<ApiResult> {
    Json(ApiResult {
        ok: true,
        message: format!("xscreen {}", env!("CARGO_PKG_VERSION")),
    })
}

async fn devices(
    State(state): State<BridgeState>,
    headers: HeaderMap,
) -> Result<Json<Vec<DeviceView>>, ApiError> {
    authorize(&state, &headers)?;
    let devices = discover_devices(state.discovery_timeout)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(
        devices
            .iter()
            .enumerate()
            .map(|(index, device)| DeviceView::from_device(index + 1, device))
            .collect(),
    ))
}

async fn cast(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    Json(request): Json<CastRequest>,
) -> Result<Json<ApiResult>, ApiError> {
    authorize(&state, &headers)?;
    let devices = discover_devices(state.discovery_timeout)
        .await
        .map_err(ApiError::internal)?;
    let device = select_device(&devices, request.device.trim())?.clone();
    let client = DlnaClient::new(device.clone()).map_err(ApiError::internal)?;
    let source = request.source.trim();
    let path = Path::new(source);
    let (media_url, mime, title, media_server) = if path.exists() {
        let server = MediaServer::start(path, 0)
            .await
            .map_err(ApiError::internal)?;
        let url = server
            .url_for_device(&device.location)
            .map_err(ApiError::internal)?;
        let mime = server.mime().to_owned();
        let title = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("xscreen media")
            .to_owned();
        (url, mime, title, Some(server))
    } else {
        let url = Url::parse(source).map_err(|_| {
            ApiError::bad_request("source is neither an existing file nor a valid media URL")
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ApiError::bad_request(
                "only local files and HTTP(S) media URLs are supported",
            ));
        }
        let mime = mime_guess::from_path(url.path())
            .first()
            .map(|mime| mime.essence_str().to_owned())
            .unwrap_or_else(|| "video/mp4".to_owned());
        let title = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|value| !value.is_empty())
            .unwrap_or("xscreen media")
            .to_owned();
        (url, mime, title, None)
    };
    let metadata = didl_metadata(media_url.as_str(), &mime, &title);
    client
        .set_transport_uri(media_url.as_str(), &metadata)
        .await
        .map_err(ApiError::internal)?;
    client.play().await.map_err(ApiError::internal)?;
    let transfer_stats = media_server.as_ref().map(MediaServer::stats);
    *state.session.write().await = Some(BridgeSession {
        client,
        _media_server: media_server,
        transfer_stats,
        source: source.to_owned(),
        device_name: device.friendly_name.clone(),
    });

    Ok(Json(ApiResult {
        ok: true,
        message: format!("casting to {}", device.friendly_name),
    }))
}

async fn control(
    State(state): State<BridgeState>,
    headers: HeaderMap,
    Json(request): Json<ControlRequest>,
) -> Result<Json<ApiResult>, ApiError> {
    authorize(&state, &headers)?;
    let client = state
        .session
        .read()
        .await
        .as_ref()
        .map(|session| session.client.clone())
        .ok_or_else(|| ApiError::bad_request("no active casting session"))?;
    let value = request.value.as_deref().unwrap_or("");
    match request.action.as_str() {
        "play" => client.play().await,
        "pause" => client.pause().await,
        "stop" => client.stop().await,
        "seek" if !value.is_empty() => client.seek(value).await,
        "rate" if !value.is_empty() => client.play_at(value).await,
        "volume" => match value.parse::<u8>() {
            Ok(volume) if volume <= 100 => client.set_volume(volume).await,
            _ => return Err(ApiError::bad_request("volume must be between 0 and 100")),
        },
        "seek" | "rate" => return Err(ApiError::bad_request("control value is required")),
        _ => return Err(ApiError::bad_request("unknown control action")),
    }
    .map_err(ApiError::internal)?;
    Ok(Json(ApiResult {
        ok: true,
        message: format!("{} command sent", request.action),
    }))
}

async fn playback_status(
    State(state): State<BridgeState>,
    headers: HeaderMap,
) -> Result<Json<PlaybackStatus>, ApiError> {
    authorize(&state, &headers)?;
    let (client, stats, source, device_name) = {
        let session = state.session.read().await;
        let session = session
            .as_ref()
            .ok_or_else(|| ApiError::bad_request("no active casting session"))?;
        (
            session.client.clone(),
            session.transfer_stats.clone(),
            session.source.clone(),
            session.device_name.clone(),
        )
    };
    let position = client.position().await.map_err(ApiError::internal)?;
    let position_text = position.position.unwrap_or_else(|| "00:00:00".to_owned());
    let duration_text = position.duration.unwrap_or_else(|| "00:00:00".to_owned());
    let snapshot = stats.map(|stats| stats.snapshot());
    Ok(Json(PlaybackStatus {
        position_seconds: parse_upnp_time(&position_text),
        duration_seconds: parse_upnp_time(&duration_text),
        position: position_text,
        duration: duration_text,
        source,
        device_name,
        bytes_sent: snapshot.map(|value| value.bytes_sent),
        file_size: snapshot.map(|value| value.file_size),
        average_mbps: snapshot.map(|value| value.average_mbps),
    }))
}

fn parse_upnp_time(value: &str) -> u64 {
    let mut parts = value.split(':');
    let hours = parts.next().and_then(|part| part.parse::<u64>().ok());
    let minutes = parts.next().and_then(|part| part.parse::<u64>().ok());
    let seconds = parts
        .next()
        .and_then(|part| part.split('.').next())
        .and_then(|part| part.parse::<u64>().ok());
    match (hours, minutes, seconds, parts.next()) {
        (Some(hours), Some(minutes), Some(seconds), None) => hours * 3600 + minutes * 60 + seconds,
        _ => 0,
    }
}

fn authorize(state: &BridgeState, headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = format!("Bearer {}", state.token);
    match headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        Some(value) if value == expected => Ok(()),
        _ => Err(ApiError::unauthorized()),
    }
}

fn select_device<'a>(devices: &'a [Device], selector: &str) -> Result<&'a Device, ApiError> {
    if devices.is_empty() {
        return Err(ApiError::bad_request("no DLNA media renderer found"));
    }
    if selector.is_empty() && devices.len() == 1 {
        return Ok(&devices[0]);
    }
    if let Ok(index) = selector.parse::<usize>()
        && let Some(device) = index.checked_sub(1).and_then(|index| devices.get(index))
    {
        return Ok(device);
    }

    let needle = selector.to_lowercase();
    let matches = devices
        .iter()
        .filter(|device| {
            device.friendly_name.to_lowercase().contains(&needle)
                || device.model_name.to_lowercase().contains(&needle)
                || device.manufacturer.to_lowercase().contains(&needle)
                || device.location.as_str().to_lowercase().contains(&needle)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [device] => Ok(*device),
        [] => Err(ApiError::bad_request(format!(
            "no discovered device matches {selector:?}"
        ))),
        _ => Err(ApiError::bad_request(format!(
            "more than one device matches {selector:?}"
        ))),
    }
}

fn didl_metadata(uri: &str, mime: &str, title: &str) -> String {
    let class = if mime.starts_with("audio/") {
        "object.item.audioItem.musicTrack"
    } else if mime.starts_with("image/") {
        "object.item.imageItem.photo"
    } else {
        "object.item.videoItem"
    };
    format!(
        r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/"><item id="0" parentID="0" restricted="1"><dc:title>{}</dc:title><upnp:class>{class}</upnp:class><res protocolInfo="http-get:*:{}:*">{}</res></item></DIDL-Lite>"#,
        escape_xml(title),
        escape_xml(mime),
        escape_xml(uri)
    )
}

pub fn default_bridge_address() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::Service;

    fn device(name: &str, address: &str) -> Device {
        Device {
            friendly_name: name.to_owned(),
            manufacturer: "Example".to_owned(),
            model_name: "TV".to_owned(),
            location: Url::parse(&format!("http://{address}/device.xml")).unwrap(),
            av_transport: Service {
                service_type: "urn:schemas-upnp-org:service:AVTransport:1".to_owned(),
                control_url: Url::parse(&format!("http://{address}/control")).unwrap(),
            },
            rendering_control: None,
        }
    }

    #[test]
    fn selects_device_by_number_name_or_ip() {
        let devices = vec![
            device("Bedroom", "192.168.1.10"),
            device("Living Room", "192.168.1.20"),
        ];
        assert_eq!(
            select_device(&devices, "2").unwrap().friendly_name,
            "Living Room"
        );
        assert_eq!(
            select_device(&devices, "bed").unwrap().friendly_name,
            "Bedroom"
        );
        assert_eq!(
            select_device(&devices, "192.168.1.20")
                .unwrap()
                .friendly_name,
            "Living Room"
        );
    }

    #[test]
    fn metadata_escapes_media_values() {
        let metadata = didl_metadata("https://host/a.m3u8?x=1&y=2", "video/mp4", "A < B");
        assert!(metadata.contains("x=1&amp;y=2"));
        assert!(metadata.contains("A &lt; B"));
    }

    #[test]
    fn parses_upnp_time_with_optional_fraction() {
        assert_eq!(parse_upnp_time("01:02:03"), 3_723);
        assert_eq!(parse_upnp_time("00:01:02.750"), 62);
        assert_eq!(parse_upnp_time("NOT_IMPLEMENTED"), 0);
    }
}
