use std::io::{self, IsTerminal, Write};
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, BufReader};
use url::Url;
use uuid::Uuid;
use xscreen::bridge::serve_bridge;
use xscreen::device::Device;
use xscreen::discovery::discover_devices;
use xscreen::media::{MediaServer, TransferStats};
use xscreen::soap::{DlnaClient, escape_xml};

#[derive(Debug, Parser)]
#[command(
    name = "xscreen",
    version,
    about = "Cast media to DLNA/UPnP televisions"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scan the local network for DLNA media renderers
    Scan {
        /// Number of seconds to wait for device responses
        #[arg(short, long, default_value_t = 3)]
        timeout: u64,
    },
    /// Cast a local media file or an HTTP(S) URL
    Cast {
        /// Local media file or HTTP(S) media URL
        source: String,
        /// Device name, model, IP address, or list number
        #[arg(short, long)]
        device: Option<String>,
        /// Number of seconds to wait for device responses
        #[arg(short, long, default_value_t = 3)]
        timeout: u64,
        /// Local port used to serve a file; 0 chooses a free port
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Load the media without starting playback
        #[arg(long)]
        no_play: bool,
    },
    /// Run the authenticated local bridge used by the browser extension
    Bridge {
        /// Loopback address for the extension bridge
        #[arg(long, default_value = "127.0.0.1")]
        listen: IpAddr,
        /// Local bridge port
        #[arg(short, long, default_value_t = 47_821)]
        port: u16,
        /// Seconds to wait while discovering televisions
        #[arg(short, long, default_value_t = 5)]
        timeout: u64,
        /// Pairing token; a random token is generated when omitted
        #[arg(long)]
        token: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Scan { timeout } => {
            let devices = scan(timeout).await?;
            print_devices(&devices);
        }
        Command::Cast {
            source,
            device,
            timeout,
            port,
            no_play,
        } => cast(&source, device.as_deref(), timeout, port, no_play).await?,
        Command::Bridge {
            listen,
            port,
            timeout,
            token,
        } => {
            if !listen.is_loopback() {
                bail!("the browser bridge may only listen on a loopback address");
            }
            let token = token.unwrap_or_else(|| Uuid::new_v4().simple().to_string());
            println!("xscreen browser bridge: http://{listen}:{port}");
            println!("pairing token: {token}");
            println!("Keep this process running while using the browser extension.");
            serve_bridge(listen, port, token, Duration::from_secs(timeout)).await?;
        }
    }
    Ok(())
}

async fn scan(timeout: u64) -> Result<Vec<Device>> {
    eprintln!("Scanning for DLNA televisions for {timeout}s...");
    let devices = discover_devices(Duration::from_secs(timeout)).await?;
    if devices.is_empty() {
        bail!(
            "no DLNA media renderer found; check that the TV is on, DLNA is enabled, and both devices are on the same non-isolated network"
        );
    }
    Ok(devices)
}

fn print_devices(devices: &[Device]) {
    for (index, device) in devices.iter().enumerate() {
        let details = [device.manufacturer.as_str(), device.model_name.as_str()]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let host = device.location.host_str().unwrap_or("unknown address");
        if details.is_empty() {
            println!("{}. {} ({host})", index + 1, device.friendly_name);
        } else {
            println!(
                "{}. {} — {} ({host})",
                index + 1,
                device.friendly_name,
                details
            );
        }
    }
}

async fn select_device(devices: Vec<Device>, selector: Option<&str>) -> Result<Device> {
    if let Some(selector) = selector {
        if let Ok(index) = selector.parse::<usize>()
            && let Some(device) = index.checked_sub(1).and_then(|index| devices.get(index))
        {
            return Ok(device.clone());
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
        return match matches.as_slice() {
            [device] => Ok((*device).clone()),
            [] => bail!("no discovered device matches {selector:?}"),
            _ => bail!("more than one device matches {selector:?}; use a more specific value"),
        };
    }

    if devices.len() == 1 {
        return Ok(devices.into_iter().next().unwrap());
    }
    if !io::stdin().is_terminal() {
        bail!("multiple devices found; choose one with --device <name-or-number>");
    }

    print_devices(&devices);
    print!("Choose a device [1-{}]: ", devices.len());
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let index: usize = input.trim().parse().context("invalid device number")?;
    devices
        .into_iter()
        .nth(
            index
                .checked_sub(1)
                .context("device number must be at least 1")?,
        )
        .context("device number is out of range")
}

async fn cast(
    source: &str,
    selector: Option<&str>,
    timeout: u64,
    port: u16,
    no_play: bool,
) -> Result<()> {
    let devices = scan(timeout).await?;
    let device = select_device(devices, selector).await?;
    eprintln!(
        "Using {} ({})",
        device.friendly_name,
        device.location.host_str().unwrap_or("?")
    );

    let path = Path::new(source);
    let (media_url, mime, title, media_server) = if path.exists() {
        let server = MediaServer::start(path, port).await?;
        let url = server.url_for_device(&device.location)?;
        let mime = server.mime().to_owned();
        let title = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("xscreen media")
            .to_owned();
        eprintln!("Serving {source} at {url}");
        (url, mime, title, Some(server))
    } else {
        let url = Url::parse(source).with_context(|| format!("media file not found: {source}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("only local files and HTTP(S) URLs are supported");
        }
        let mime = mime_guess::from_path(url.path())
            .first()
            .map(|mime| mime.essence_str().to_owned())
            .unwrap_or_else(|| "video/mp4".to_owned());
        let title = url
            .path_segments()
            .and_then(|mut parts| parts.next_back())
            .filter(|value| !value.is_empty())
            .unwrap_or("xscreen media")
            .to_owned();
        (url, mime, title, None)
    };

    let client = DlnaClient::new(device)?;
    let metadata = didl_metadata(media_url.as_str(), &mime, &title);
    client
        .set_transport_uri(media_url.as_str(), &metadata)
        .await
        .context("the TV rejected the media URL")?;
    if !no_play {
        client
            .play()
            .await
            .context("the TV could not start playback")?;
    }
    println!("Connected. Type 'help' for controls; 'quit' stops playback and exits.");
    let transfer_stats = media_server.as_ref().map(MediaServer::stats);
    control_loop(client, transfer_stats).await
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

async fn control_loop(client: DlnaClient, transfer_stats: Option<TransferStats>) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("xscreen> ");
        io::stdout().flush()?;
        let line = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = client.stop().await;
                println!();
                return Ok(());
            }
            line = lines.next_line() => line?,
        };
        let Some(line) = line else {
            let _ = client.stop().await;
            return Ok(());
        };
        let mut parts = line.split_whitespace();
        let Some(command) = parts.next().map(str::to_ascii_lowercase) else {
            continue;
        };
        let result = match command.as_str() {
            "play" | "resume" => client.play().await,
            "rate" => match parts.next().map(normalize_play_speed).transpose() {
                Ok(Some(speed)) => client.play_at(&speed).await,
                Ok(None) => Err(anyhow::anyhow!("usage: rate <number|fraction>")),
                Err(error) => Err(error),
            },
            "pause" => client.pause().await,
            "stop" => client.stop().await,
            "seek" => match parts.next().map(parse_time).transpose() {
                Ok(Some(target)) => client.seek(&target).await,
                Ok(None) => Err(anyhow::anyhow!("usage: seek <seconds|HH:MM:SS>")),
                Err(error) => Err(error),
            },
            "volume" | "vol" => match parts.next().and_then(|value| value.parse::<u8>().ok()) {
                Some(volume) if volume <= 100 => client.set_volume(volume).await,
                _ => Err(anyhow::anyhow!("usage: volume <0-100>")),
            },
            "position" | "pos" => match client.position().await {
                Ok(info) => {
                    println!(
                        "{} / {}",
                        info.position.as_deref().unwrap_or("unknown"),
                        info.duration.as_deref().unwrap_or("unknown")
                    );
                    Ok(())
                }
                Err(error) => Err(error),
            },
            "stats" | "speed" => {
                if let Some(stats) = &transfer_stats {
                    let snapshot = stats.snapshot();
                    println!(
                        "sent {} / {} in {:.1}s, average {:.2} Mbps",
                        format_bytes(snapshot.bytes_sent),
                        format_bytes(snapshot.file_size),
                        snapshot.elapsed.as_secs_f64(),
                        snapshot.average_mbps
                    );
                } else {
                    println!(
                        "network URLs are fetched directly by the TV; xscreen cannot measure that traffic"
                    );
                }
                Ok(())
            }
            "help" | "?" => {
                println!(
                    "play | pause | stop | rate <0.5|1|1.5|2> | seek <seconds|HH:MM:SS> | volume <0-100> | position | stats | quit"
                );
                Ok(())
            }
            "quit" | "exit" | "q" => {
                let _ = client.stop().await;
                return Ok(());
            }
            _ => Err(anyhow::anyhow!("unknown command; type 'help'")),
        };
        if let Err(error) = result {
            eprintln!("error: {error:#}");
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    if bytes as f64 >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.2} MiB", bytes as f64 / MIB)
    }
}

fn normalize_play_speed(value: &str) -> Result<String> {
    let value = value.trim();
    if let Some((numerator, denominator)) = value.split_once('/') {
        let numerator: u64 = numerator.parse().context("invalid playback speed")?;
        let denominator: u64 = denominator.parse().context("invalid playback speed")?;
        if numerator == 0 || denominator == 0 {
            bail!("playback speed must be greater than zero");
        }
        let divisor = greatest_common_divisor(numerator, denominator);
        return Ok(format!("{}/{}", numerator / divisor, denominator / divisor));
    }

    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 6
    {
        bail!("invalid playback speed; use a positive number such as 1.5 or 2");
    }
    let denominator = 10_u64.pow(fraction.len() as u32);
    let whole: u64 = whole.parse().context("invalid playback speed")?;
    let fraction: u64 = if fraction.is_empty() {
        0
    } else {
        fraction.parse().context("invalid playback speed")?
    };
    let numerator = whole
        .checked_mul(denominator)
        .and_then(|value| value.checked_add(fraction))
        .context("playback speed is too large")?;
    if numerator == 0 {
        bail!("playback speed must be greater than zero");
    }
    let divisor = greatest_common_divisor(numerator, denominator);
    let numerator = numerator / divisor;
    let denominator = denominator / divisor;
    if denominator == 1 {
        Ok(numerator.to_string())
    } else {
        Ok(format!("{numerator}/{denominator}"))
    }
}

fn greatest_common_divisor(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn parse_time(value: &str) -> Result<String> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Ok(format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            (seconds % 3600) / 60,
            seconds % 60
        ));
    }
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() == 3
        && parts.iter().all(|part| part.parse::<u64>().is_ok())
        && parts[1].parse::<u64>()? < 60
        && parts[2].parse::<u64>()? < 60
    {
        return Ok(format!(
            "{:02}:{:02}:{:02}",
            parts[0].parse::<u64>()?,
            parts[1].parse::<u64>()?,
            parts[2].parse::<u64>()?
        ));
    }
    bail!("invalid time; use seconds or HH:MM:SS")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seek_times() {
        assert_eq!(parse_time("65").unwrap(), "00:01:05");
        assert_eq!(parse_time("1:02:03").unwrap(), "01:02:03");
        assert!(parse_time("1:99:00").is_err());
    }

    #[test]
    fn metadata_is_validly_escaped() {
        let metadata = didl_metadata("http://host/a?x=1&y=2", "video/mp4", "A < B");
        assert!(metadata.contains("A &lt; B"));
        assert!(metadata.contains("x=1&amp;y=2"));
    }

    #[test]
    fn normalizes_playback_speed_as_a_rational_number() {
        assert_eq!(normalize_play_speed("2").unwrap(), "2");
        assert_eq!(normalize_play_speed("1.5").unwrap(), "3/2");
        assert_eq!(normalize_play_speed("0.5").unwrap(), "1/2");
        assert_eq!(normalize_play_speed("6/4").unwrap(), "3/2");
        assert!(normalize_play_speed("0").is_err());
        assert!(normalize_play_speed("fast").is_err());
    }
}
