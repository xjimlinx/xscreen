use std::time::Duration;

use anyhow::{Context, Result, bail};
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::device::{Device, Service};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionInfo {
    pub duration: Option<String>,
    pub position: Option<String>,
}

#[derive(Clone)]
pub struct DlnaClient {
    client: reqwest::Client,
    device: Device,
}

impl DlnaClient {
    pub fn new(device: Device) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .context("failed to create the DLNA HTTP client")?;
        Ok(Self { client, device })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub async fn set_transport_uri(&self, uri: &str, metadata: &str) -> Result<()> {
        self.call(
            &self.device.av_transport,
            "SetAVTransportURI",
            &[
                ("InstanceID", "0"),
                ("CurrentURI", uri),
                ("CurrentURIMetaData", metadata),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn play(&self) -> Result<()> {
        self.play_at("1").await
    }

    pub async fn play_at(&self, speed: &str) -> Result<()> {
        self.call(
            &self.device.av_transport,
            "Play",
            &[("InstanceID", "0"), ("Speed", speed)],
        )
        .await?;
        Ok(())
    }

    pub async fn pause(&self) -> Result<()> {
        self.call(&self.device.av_transport, "Pause", &[("InstanceID", "0")])
            .await?;
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        self.call(&self.device.av_transport, "Stop", &[("InstanceID", "0")])
            .await?;
        Ok(())
    }

    pub async fn seek(&self, target: &str) -> Result<()> {
        self.call(
            &self.device.av_transport,
            "Seek",
            &[
                ("InstanceID", "0"),
                ("Unit", "REL_TIME"),
                ("Target", target),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn position(&self) -> Result<PositionInfo> {
        let response = self
            .call(
                &self.device.av_transport,
                "GetPositionInfo",
                &[("InstanceID", "0")],
            )
            .await?;
        Ok(PositionInfo {
            duration: xml_text(&response, b"TrackDuration"),
            position: xml_text(&response, b"RelTime"),
        })
    }

    pub async fn set_volume(&self, volume: u8) -> Result<()> {
        let service = self
            .device
            .rendering_control
            .as_ref()
            .context("this device has no RenderingControl service")?;
        let desired = volume.min(100).to_string();
        self.call(
            service,
            "SetVolume",
            &[
                ("InstanceID", "0"),
                ("Channel", "Master"),
                ("DesiredVolume", &desired),
            ],
        )
        .await?;
        Ok(())
    }

    async fn call(
        &self,
        service: &Service,
        action: &str,
        arguments: &[(&str, &str)],
    ) -> Result<String> {
        let body = soap_envelope(&service.service_type, action, arguments);
        let response = self
            .client
            .post(service.control_url.clone())
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header(
                "SOAPAction",
                format!("\"{}#{action}\"", service.service_type),
            )
            .body(body)
            .send()
            .await
            .with_context(|| format!("DLNA {action} request failed"))?;
        let status = response.status();
        let response_body = response
            .text()
            .await
            .with_context(|| format!("failed to read the DLNA {action} response"))?;

        if !status.is_success() {
            let code = xml_text(&response_body, b"errorCode").unwrap_or_else(|| status.to_string());
            let description = xml_text(&response_body, b"errorDescription")
                .unwrap_or_else(|| "unknown UPnP error".to_owned());
            bail!("DLNA {action} failed: {code} {description}");
        }
        Ok(response_body)
    }
}

pub fn soap_envelope(service_type: &str, action: &str, arguments: &[(&str, &str)]) -> String {
    let mut body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action} xmlns:u="{}">"#,
        escape_xml(service_type)
    );
    for (name, value) in arguments {
        body.push('<');
        body.push_str(name);
        body.push('>');
        body.push_str(&escape_xml(value));
        body.push_str("</");
        body.push_str(name);
        body.push('>');
    }
    body.push_str(&format!("</u:{action}></s:Body></s:Envelope>"));
    body
}

pub fn escape_xml(value: &str) -> String {
    quick_xml::escape::escape(value).into_owned()
}

fn xml_text(xml: &str, wanted: &[u8]) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) if start.name().local_name().as_ref() == wanted => {
                return reader
                    .read_text(start.name())
                    .ok()
                    .map(|value| value.into_owned());
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_soap_argument_values() {
        let xml = soap_envelope(
            "urn:test&service",
            "SetURI",
            &[("URI", "http://host/video?a=1&b=<two>")],
        );
        assert!(xml.contains("urn:test&amp;service"));
        assert!(xml.contains("a=1&amp;b=&lt;two&gt;"));
    }

    #[test]
    fn extracts_namespaced_xml_text() {
        let xml = r#"<s:Envelope><s:Body><u:GetPositionInfoResponse><TrackDuration>01:02:03</TrackDuration><RelTime>00:03:04</RelTime></u:GetPositionInfoResponse></s:Body></s:Envelope>"#;
        assert_eq!(xml_text(xml, b"RelTime").as_deref(), Some("00:03:04"));
    }
}
