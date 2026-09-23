use anyhow::{Context, Result, bail};
use serde::Deserialize;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub service_type: String,
    pub control_url: Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub friendly_name: String,
    pub manufacturer: String,
    pub model_name: String,
    pub location: Url,
    pub av_transport: Service,
    pub rendering_control: Option<Service>,
}

#[derive(Debug, Deserialize)]
struct RootDescription {
    #[serde(rename = "URLBase")]
    url_base: Option<String>,
    device: DeviceDescription,
}

#[derive(Debug, Deserialize)]
struct DeviceDescription {
    #[serde(default, rename = "friendlyName")]
    friendly_name: String,
    #[serde(default)]
    manufacturer: String,
    #[serde(default, rename = "modelName")]
    model_name: String,
    #[serde(default, rename = "serviceList")]
    service_list: ServiceList,
    #[serde(default, rename = "deviceList")]
    device_list: DeviceList,
}

#[derive(Debug, Default, Deserialize)]
struct ServiceList {
    #[serde(default, rename = "service")]
    services: Vec<ServiceDescription>,
}

#[derive(Debug, Default, Deserialize)]
struct DeviceList {
    #[serde(default, rename = "device")]
    devices: Vec<DeviceDescription>,
}

#[derive(Debug, Deserialize)]
struct ServiceDescription {
    #[serde(rename = "serviceType")]
    service_type: String,
    #[serde(rename = "controlURL")]
    control_url: String,
}

pub async fn fetch_device(client: &reqwest::Client, location: Url) -> Result<Device> {
    let xml = client
        .get(location.clone())
        .send()
        .await
        .with_context(|| format!("failed to fetch device description from {location}"))?
        .error_for_status()
        .with_context(|| format!("device description request failed for {location}"))?
        .text()
        .await
        .context("failed to read the device description")?;
    parse_device_description(&xml, location)
}

pub fn parse_device_description(xml: &str, location: Url) -> Result<Device> {
    let root: RootDescription =
        quick_xml::de::from_str(xml).context("invalid UPnP device description XML")?;
    let base = match root
        .url_base
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(value) => Url::parse(value).context("invalid URLBase in device description")?,
        None => location.clone(),
    };

    let mut services = Vec::new();
    collect_services(&root.device, &mut services);
    let av = services
        .iter()
        .find(|service| service.service_type.contains(":service:AVTransport:"))
        .context("device does not advertise an AVTransport service")?;
    let rendering = services
        .iter()
        .find(|service| service.service_type.contains(":service:RenderingControl:"));

    let resolve = |service: &ServiceDescription| -> Result<Service> {
        let control_url = base
            .join(service.control_url.trim())
            .with_context(|| format!("invalid control URL: {}", service.control_url))?;
        Ok(Service {
            service_type: service.service_type.clone(),
            control_url,
        })
    };

    let friendly_name = root.device.friendly_name.trim().to_owned();
    if friendly_name.is_empty() {
        bail!("device description has no friendlyName");
    }

    Ok(Device {
        friendly_name,
        manufacturer: root.device.manufacturer.trim().to_owned(),
        model_name: root.device.model_name.trim().to_owned(),
        location,
        av_transport: resolve(av)?,
        rendering_control: rendering.map(|service| resolve(service)).transpose()?,
    })
}

fn collect_services<'a>(device: &'a DeviceDescription, output: &mut Vec<&'a ServiceDescription>) {
    output.extend(device.service_list.services.iter());
    for child in &device.device_list.devices {
        collect_services(child, output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_resolves_device_services() {
        let xml = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <URLBase>http://192.168.1.8:8080/base/</URLBase>
  <device>
    <friendlyName>Living Room TV</friendlyName>
    <manufacturer>Example</manufacturer>
    <modelName>TV 1</modelName>
    <serviceList>
      <service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>/upnp/control/av</controlURL></service>
      <service><serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType><controlURL>volume</controlURL></service>
    </serviceList>
  </device>
</root>"#;
        let device =
            parse_device_description(xml, Url::parse("http://192.168.1.8/device.xml").unwrap())
                .unwrap();
        assert_eq!(device.friendly_name, "Living Room TV");
        assert_eq!(
            device.av_transport.control_url.as_str(),
            "http://192.168.1.8:8080/upnp/control/av"
        );
        assert_eq!(
            device.rendering_control.unwrap().control_url.as_str(),
            "http://192.168.1.8:8080/base/volume"
        );
    }

    #[test]
    fn finds_services_on_embedded_devices() {
        let xml = r#"<root><device><friendlyName>TV</friendlyName><deviceList><device><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:2</serviceType><controlURL>/av</controlURL></service></serviceList></device></deviceList></device></root>"#;
        let device =
            parse_device_description(xml, Url::parse("http://10.0.0.2/description.xml").unwrap())
                .unwrap();
        assert!(device.av_transport.service_type.ends_with(":2"));
    }
}
