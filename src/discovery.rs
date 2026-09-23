use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout_at};
use url::Url;

use crate::device::{Device, fetch_device};

const SSDP_ADDRESS: &str = "239.255.255.250:1900";
const SEARCH_TARGETS: &[&str] = &[
    "urn:schemas-upnp-org:device:MediaRenderer:1",
    "urn:schemas-upnp-org:service:AVTransport:1",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsdpResponse {
    pub location: Url,
    pub usn: Option<String>,
    pub server: Option<String>,
}

pub async fn discover_devices(wait: Duration) -> Result<Vec<Device>> {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("failed to bind the SSDP discovery socket")?;
    socket
        .set_broadcast(true)
        .context("failed to configure the SSDP socket")?;

    for _ in 0..2 {
        for target in SEARCH_TARGETS {
            let request = format!(
                "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_ADDRESS}\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {target}\r\n\r\n"
            );
            socket
                .send_to(request.as_bytes(), SSDP_ADDRESS)
                .await
                .context("failed to send the SSDP discovery request")?;
        }
    }

    let deadline = Instant::now() + wait;
    let mut buffer = vec![0_u8; 65_535];
    let mut responses = HashMap::<String, SsdpResponse>::new();

    loop {
        let received = timeout_at(deadline, socket.recv_from(&mut buffer)).await;
        let Ok(result) = received else { break };
        let (length, _) = result.context("failed while receiving an SSDP response")?;
        let Some(response) = parse_ssdp_response(&buffer[..length]) else {
            continue;
        };
        responses
            .entry(response.location.to_string())
            .or_insert(response);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("failed to create the HTTP client")?;
    let mut tasks = tokio::task::JoinSet::new();

    for response in responses.into_values() {
        let client = client.clone();
        tasks.spawn(async move { fetch_device(&client, response.location).await });
    }

    let mut devices = Vec::new();
    while let Some(result) = tasks.join_next().await {
        if let Ok(Ok(device)) = result {
            devices.push(device);
        }
    }
    devices.sort_by_key(|device| device.friendly_name.to_lowercase());
    devices.dedup_by(|left, right| left.location == right.location);
    Ok(devices)
}

pub fn parse_ssdp_response(bytes: &[u8]) -> Option<SsdpResponse> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines();
    let status = lines.next()?.trim();
    if !status.starts_with("HTTP/1.1 200") && !status.starts_with("HTTP/1.0 200") {
        return None;
    }

    let mut headers = HashMap::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }

    let location = Url::parse(headers.get("location")?).ok()?;
    Some(SsdpResponse {
        location,
        usn: headers.get("usn").cloned(),
        server: headers.get("server").cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_case_insensitive_ssdp_headers() {
        let packet = b"HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.9:1400/xml/device.xml\r\nUsn: uuid:abc::urn:test\r\nSERVER: test/1.0\r\n\r\n";
        let response = parse_ssdp_response(packet).unwrap();
        assert_eq!(response.location.host_str(), Some("192.168.1.9"));
        assert_eq!(response.usn.as_deref(), Some("uuid:abc::urn:test"));
        assert_eq!(response.server.as_deref(), Some("test/1.0"));
    }

    #[test]
    fn rejects_notifications() {
        assert!(parse_ssdp_response(b"NOTIFY * HTTP/1.1\r\nLOCATION: http://tv/\r\n").is_none());
    }
}
