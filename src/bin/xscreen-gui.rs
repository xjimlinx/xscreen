use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, RichText};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:47821";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceInfo {
    index: usize,
    friendly_name: String,
    manufacturer: String,
    model_name: String,
    address: String,
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Deserialize)]
struct ApiResult {
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CastRequest<'a> {
    source: &'a str,
    device: &'a str,
}

#[derive(Debug, Serialize)]
struct ControlRequest<'a> {
    action: &'a str,
    value: Option<&'a str>,
}

enum GuiMessage {
    Devices(Result<Vec<DeviceInfo>, String>),
    Action {
        starts_session: bool,
        result: Result<String, String>,
    },
    Playback(Result<PlaybackStatus, String>),
    Preview(Result<Vec<u8>, String>),
}

struct XscreenGui {
    endpoint: String,
    token: String,
    devices: Vec<DeviceInfo>,
    selected_device: usize,
    source: String,
    status_text: String,
    status_is_error: bool,
    busy: bool,
    has_session: bool,
    position_seconds: f64,
    duration_seconds: f64,
    position_text: String,
    duration_text: String,
    volume: f64,
    rate_index: usize,
    current_source: String,
    current_device: String,
    bytes_sent: Option<u64>,
    file_size: Option<u64>,
    average_mbps: Option<f64>,
    preview: Option<egui::TextureHandle>,
    preview_error: Option<String>,
    sender: Sender<GuiMessage>,
    receiver: Receiver<GuiMessage>,
    last_poll: Instant,
    poll_pending: bool,
    dragging_progress: bool,
}

impl XscreenGui {
    fn new(context: &eframe::CreationContext<'_>) -> Self {
        install_chinese_font(&context.egui_ctx);
        let mut style = (*context.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(10.0, 9.0);
        style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(7);
        style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(7);
        style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(7);
        context.egui_ctx.set_style(style);
        let (sender, receiver) = mpsc::channel();
        Self {
            endpoint: DEFAULT_ENDPOINT.to_owned(),
            token: String::new(),
            devices: Vec::new(),
            selected_device: 0,
            source: String::new(),
            status_text: "请先启动 xscreen bridge，然后填写配对令牌。".to_owned(),
            status_is_error: false,
            busy: false,
            has_session: false,
            position_seconds: 0.0,
            duration_seconds: 0.0,
            position_text: "00:00:00".to_owned(),
            duration_text: "00:00:00".to_owned(),
            volume: 50.0,
            rate_index: 2,
            current_source: String::new(),
            current_device: String::new(),
            bytes_sent: None,
            file_size: None,
            average_mbps: None,
            preview: None,
            preview_error: None,
            sender,
            receiver,
            last_poll: Instant::now(),
            poll_pending: false,
            dragging_progress: false,
        }
    }

    fn validate_connection(&mut self) -> bool {
        if self.endpoint.trim().is_empty() {
            self.show_error("请填写服务地址");
            return false;
        }
        if self.token.trim().is_empty() {
            self.show_error("请填写 xscreen bridge 输出的配对令牌");
            return false;
        }
        true
    }

    fn show_error(&mut self, message: impl Into<String>) {
        self.status_text = message.into();
        self.status_is_error = true;
    }

    fn show_status(&mut self, message: impl Into<String>) {
        self.status_text = message.into();
        self.status_is_error = false;
    }

    fn selected_device_info(&self) -> Option<&DeviceInfo> {
        self.devices.get(self.selected_device)
    }

    fn refresh_devices(&mut self, context: &egui::Context) {
        if !self.validate_connection() || self.busy {
            return;
        }
        self.busy = true;
        self.show_status("正在扫描局域网电视…");
        let endpoint = self.endpoint.trim_end_matches('/').to_owned();
        let token = self.token.trim().to_owned();
        let sender = self.sender.clone();
        let context = context.clone();
        thread::spawn(move || {
            let result = get_json::<Vec<DeviceInfo>>(&endpoint, "/api/devices", &token);
            let _ = sender.send(GuiMessage::Devices(result));
            context.request_repaint();
        });
    }

    fn cast(&mut self, context: &egui::Context) {
        if !self.validate_connection() || self.busy {
            return;
        }
        let Some(device) = self
            .selected_device_info()
            .map(|device| device.index.to_string())
        else {
            self.show_error("请先扫描并选择电视");
            return;
        };
        let source = self.source.trim().to_owned();
        if source.is_empty() {
            self.show_error("请选择本地文件或填写网络媒体 URL");
            return;
        }
        self.busy = true;
        self.show_status("正在连接电视并发送媒体…");
        let endpoint = self.endpoint.trim_end_matches('/').to_owned();
        let token = self.token.trim().to_owned();
        let sender = self.sender.clone();
        let context = context.clone();
        thread::spawn(move || {
            let body = CastRequest {
                source: &source,
                device: &device,
            };
            let result = post_json::<_, ApiResult>(&endpoint, "/api/cast", &token, &body)
                .map(|response| response.message);
            let _ = sender.send(GuiMessage::Action {
                starts_session: true,
                result,
            });
            context.request_repaint();
        });
    }

    fn send_control(
        &mut self,
        context: &egui::Context,
        action: &'static str,
        value: Option<String>,
    ) {
        if !self.validate_connection() || !self.has_session {
            if !self.has_session {
                self.show_error("当前没有投屏会话");
            }
            return;
        }
        let endpoint = self.endpoint.trim_end_matches('/').to_owned();
        let token = self.token.trim().to_owned();
        let sender = self.sender.clone();
        let context = context.clone();
        thread::spawn(move || {
            let body = ControlRequest {
                action,
                value: value.as_deref(),
            };
            let result = post_json::<_, ApiResult>(&endpoint, "/api/control", &token, &body)
                .map(|response| response.message);
            let _ = sender.send(GuiMessage::Action {
                starts_session: false,
                result,
            });
            context.request_repaint();
        });
    }

    fn poll_playback(&mut self, context: &egui::Context) {
        if !self.has_session
            || self.poll_pending
            || self.dragging_progress
            || self.last_poll.elapsed() < Duration::from_secs(1)
        {
            return;
        }
        self.poll_pending = true;
        self.last_poll = Instant::now();
        let endpoint = self.endpoint.trim_end_matches('/').to_owned();
        let token = self.token.trim().to_owned();
        let sender = self.sender.clone();
        let context = context.clone();
        thread::spawn(move || {
            let result = get_json::<PlaybackStatus>(&endpoint, "/api/status", &token);
            let _ = sender.send(GuiMessage::Playback(result));
            context.request_repaint();
        });
    }

    fn choose_file(&mut self, context: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("视频", &["mp4", "mkv", "ts", "webm", "mov", "m4v"])
            .add_filter("音频", &["mp3", "m4a", "aac", "flac", "wav"])
            .pick_file()
        {
            self.source = path.display().to_string();
            self.generate_preview(context);
        }
    }

    fn generate_preview(&mut self, context: &egui::Context) {
        let source = self.source.trim().to_owned();
        if source.is_empty() || source.starts_with("blob:") {
            self.preview = None;
            self.preview_error = Some("该媒体地址无法生成预览".to_owned());
            return;
        }
        self.preview = None;
        self.preview_error = None;
        self.show_status("正在生成预览图…");
        let sender = self.sender.clone();
        let context = context.clone();
        thread::spawn(move || {
            let output = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-ss",
                    "3",
                    "-i",
                    &source,
                    "-frames:v",
                    "1",
                    "-vf",
                    "scale=720:-2",
                    "-f",
                    "image2pipe",
                    "-vcodec",
                    "png",
                    "pipe:1",
                ])
                .output();
            let result = match output {
                Ok(output) if output.status.success() && !output.stdout.is_empty() => {
                    Ok(output.stdout)
                }
                Ok(output) => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
                Err(error) => Err(format!("无法启动 ffmpeg：{error}")),
            };
            let _ = sender.send(GuiMessage::Preview(result));
            context.request_repaint();
        });
    }

    fn receive_messages(&mut self, context: &egui::Context) {
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                GuiMessage::Devices(result) => {
                    self.busy = false;
                    match result {
                        Ok(devices) if devices.is_empty() => {
                            self.devices.clear();
                            self.show_error("没有发现电视，请检查网络与 DLNA 设置");
                        }
                        Ok(devices) => {
                            let count = devices.len();
                            self.devices = devices;
                            self.selected_device = self
                                .selected_device
                                .min(self.devices.len().saturating_sub(1));
                            self.show_status(format!("发现 {count} 台可用设备"));
                        }
                        Err(error) => self.show_error(error),
                    }
                }
                GuiMessage::Action {
                    starts_session,
                    result,
                } => {
                    self.busy = false;
                    match result {
                        Ok(message) => {
                            if starts_session {
                                self.has_session = true;
                                self.last_poll = Instant::now() - Duration::from_secs(2);
                            }
                            self.show_status(message);
                        }
                        Err(error) => self.show_error(error),
                    }
                }
                GuiMessage::Playback(result) => {
                    self.poll_pending = false;
                    if let Ok(playback) = result {
                        if !self.dragging_progress {
                            self.position_seconds = playback.position_seconds as f64;
                        }
                        self.duration_seconds = playback.duration_seconds as f64;
                        self.position_text = playback.position;
                        self.duration_text = playback.duration;
                        self.current_source = playback.source;
                        self.current_device = playback.device_name;
                        self.bytes_sent = playback.bytes_sent;
                        self.file_size = playback.file_size;
                        self.average_mbps = playback.average_mbps;
                    }
                }
                GuiMessage::Preview(result) => match result {
                    Ok(bytes) => match image::load_from_memory(&bytes) {
                        Ok(image) => {
                            let image = image.to_rgba8();
                            let size = [image.width() as usize, image.height() as usize];
                            let color_image =
                                egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
                            self.preview = Some(context.load_texture(
                                "media-preview",
                                color_image,
                                egui::TextureOptions::LINEAR,
                            ));
                            self.preview_error = None;
                            self.show_status("预览图已生成");
                        }
                        Err(error) => {
                            self.preview_error = Some(format!("预览图解析失败：{error}"));
                        }
                    },
                    Err(error) => {
                        self.preview_error = Some(if error.is_empty() {
                            "无法生成预览图".to_owned()
                        } else {
                            error
                        });
                    }
                },
            }
        }
    }

    fn connection_panel(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        ui.group(|ui| {
            ui.heading("连接设置");
            egui::Grid::new("connection-grid")
                .num_columns(2)
                .spacing([12.0, 9.0])
                .show(ui, |ui| {
                    ui.label("服务地址");
                    ui.text_edit_singleline(&mut self.endpoint);
                    ui.end_row();
                    ui.label("配对令牌");
                    ui.add(egui::TextEdit::singleline(&mut self.token).password(true));
                    ui.end_row();
                });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.busy, egui::Button::new("扫描电视"))
                    .clicked()
                {
                    self.refresh_devices(context);
                }
                if !self.devices.is_empty() {
                    let selected = self
                        .selected_device_info()
                        .map(|device| device.friendly_name.clone())
                        .unwrap_or_else(|| "请选择设备".to_owned());
                    egui::ComboBox::from_id_salt("device-combo")
                        .selected_text(selected)
                        .width(360.0)
                        .show_ui(ui, |ui| {
                            for (index, device) in self.devices.iter().enumerate() {
                                let details = format!(
                                    "{}. {} — {} {} ({})",
                                    device.index,
                                    device.friendly_name,
                                    device.manufacturer,
                                    device.model_name,
                                    device.address
                                );
                                ui.selectable_value(&mut self.selected_device, index, details);
                            }
                        });
                }
            });
        });
    }

    fn source_panel(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        ui.group(|ui| {
            ui.heading("媒体来源");
            ui.horizontal(|ui| {
                ui.add_sized(
                    [ui.available_width() - 188.0, 30.0],
                    egui::TextEdit::singleline(&mut self.source)
                        .hint_text("本地文件路径或 HTTP(S) 媒体 URL"),
                );
                if ui.button("选择文件").clicked() {
                    self.choose_file(context);
                }
                if ui.button("预览").clicked() {
                    self.generate_preview(context);
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.busy,
                        egui::Button::new(RichText::new("投送到电视").strong()),
                    )
                    .clicked()
                {
                    self.cast(context);
                }
                ui.label(
                    RichText::new(
                        "网页中的 blob: 地址不能直接投送，请使用浏览器扩展寻找真实媒体地址。",
                    )
                    .small()
                    .color(Color32::GRAY),
                );
            });
        });
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("媒体预览");
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 220.0),
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
                |ui| {
                    if let Some(texture) = &self.preview {
                        let available = ui.available_size();
                        ui.add(
                            egui::Image::new(texture)
                                .fit_to_exact_size(fit_size(texture.size_vec2(), available)),
                        );
                    } else if let Some(error) = &self.preview_error {
                        ui.label(RichText::new(error).color(Color32::LIGHT_RED));
                    } else {
                        ui.label(RichText::new("选择媒体后点击“预览”").color(Color32::GRAY));
                    }
                },
            );
        });
    }

    fn player_panel(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        const RATES: &[(&str, &str)] = &[
            ("0.5×", "1/2"),
            ("0.75×", "3/4"),
            ("1.0×", "1"),
            ("1.25×", "5/4"),
            ("1.5×", "3/2"),
            ("2.0×", "2"),
        ];
        ui.add_enabled_ui(self.has_session, |ui| {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.heading("播放控制");
                    if !self.current_device.is_empty() {
                        ui.label(
                            RichText::new(format!("正在连接：{}", self.current_device))
                                .small()
                                .color(Color32::GRAY),
                        );
                    }
                });
                let maximum = self.duration_seconds.max(1.0);
                let response = ui.add(
                    egui::Slider::new(&mut self.position_seconds, 0.0..=maximum)
                        .show_value(false)
                        .text("播放进度"),
                );
                self.dragging_progress = response.dragged();
                if response.drag_stopped() || (response.changed() && !response.dragged()) {
                    self.send_control(
                        context,
                        "seek",
                        Some(format_time(self.position_seconds as u64)),
                    );
                }
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{} / {}",
                        if self.dragging_progress {
                            format_time(self.position_seconds as u64)
                        } else {
                            self.position_text.clone()
                        },
                        if self.duration_seconds > 0.0 {
                            format_time(self.duration_seconds as u64)
                        } else {
                            self.duration_text.clone()
                        }
                    ));
                    ui.separator();
                    if ui.button("播放").clicked() {
                        self.send_control(context, "play", None);
                    }
                    if ui.button("暂停").clicked() {
                        self.send_control(context, "pause", None);
                    }
                    if ui.button("停止").clicked() {
                        self.send_control(context, "stop", None);
                    }
                    ui.separator();
                    ui.label("倍速");
                    let old_rate = self.rate_index;
                    egui::ComboBox::from_id_salt("rate-combo")
                        .selected_text(RATES[self.rate_index].0)
                        .show_ui(ui, |ui| {
                            for (index, (label, _)) in RATES.iter().enumerate() {
                                ui.selectable_value(&mut self.rate_index, index, *label);
                            }
                        });
                    if self.rate_index != old_rate {
                        self.send_control(
                            context,
                            "rate",
                            Some(RATES[self.rate_index].1.to_owned()),
                        );
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("音量");
                    let volume_response = ui.add(
                        egui::Slider::new(&mut self.volume, 0.0..=100.0)
                            .integer()
                            .suffix("%"),
                    );
                    if volume_response.drag_stopped()
                        || (volume_response.changed() && !volume_response.dragged())
                    {
                        self.send_control(
                            context,
                            "volume",
                            Some((self.volume.round() as u8).to_string()),
                        );
                    }
                    if let (Some(bytes), Some(size)) = (self.bytes_sent, self.file_size) {
                        ui.separator();
                        ui.label(format!(
                            "已传输 {} / {} · {:.2} Mbps",
                            format_bytes(bytes),
                            format_bytes(size),
                            self.average_mbps.unwrap_or(0.0)
                        ));
                    }
                });
            });
        });
    }
}

impl eframe::App for XscreenGui {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_messages(context);
        self.poll_playback(context);
        context.request_repaint_after(Duration::from_millis(250));

        egui::TopBottomPanel::top("header").show(context, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("xscreen 投屏控制台").size(24.0));
                ui.separator();
                ui.label("DLNA 媒体投送与播放控制");
                if self.busy {
                    ui.spinner();
                }
            });
            ui.add_space(6.0);
        });

        egui::TopBottomPanel::bottom("status").show(context, |ui| {
            let color = if self.status_is_error {
                Color32::LIGHT_RED
            } else {
                Color32::from_rgb(80, 190, 130)
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(color));
                ui.label(&self.status_text);
                if !self.current_source.is_empty() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let name = Path::new(&self.current_source)
                            .file_name()
                            .and_then(|value| value.to_str())
                            .unwrap_or(&self.current_source);
                        ui.label(RichText::new(name).small().color(Color32::GRAY));
                    });
                }
            });
        });

        egui::CentralPanel::default().show(context, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.connection_panel(ui, context);
                ui.add_space(8.0);
                self.source_panel(ui, context);
                ui.add_space(8.0);
                self.player_panel(ui, context);
                ui.add_space(8.0);
                self.preview_panel(ui);
            });
        });
    }
}

fn install_chinese_font(context: &egui::Context) {
    let candidates = [
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/WindowsFonts/msyh.ttc",
    ];
    let Some((_, bytes)) = candidates
        .iter()
        .find_map(|path| fs::read(path).ok().map(|bytes| (*path, bytes)))
    else {
        return;
    };
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "xscreen-cjk".to_owned(),
        Arc::new(FontData::from_owned(bytes)),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "xscreen-cjk".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("xscreen-cjk".to_owned());
    context.set_fonts(fonts);
}

fn get_json<T: for<'de> Deserialize<'de>>(
    endpoint: &str,
    path: &str,
    token: &str,
) -> Result<T, String> {
    let response = http_client()?
        .get(format!("{endpoint}{path}"))
        .bearer_auth(token)
        .send()
        .map_err(|error| format!("无法连接 xscreen bridge：{error}"))?;
    parse_response(response)
}

fn post_json<B: Serialize, T: for<'de> Deserialize<'de>>(
    endpoint: &str,
    path: &str,
    token: &str,
    body: &B,
) -> Result<T, String> {
    let response = http_client()?
        .post(format!("{endpoint}{path}"))
        .bearer_auth(token)
        .json(body)
        .send()
        .map_err(|error| format!("无法连接 xscreen bridge：{error}"))?;
    parse_response(response)
}

fn http_client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(20))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())
}

fn parse_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::blocking::Response,
) -> Result<T, String> {
    let status = response.status();
    let body = response.text().map_err(|error| error.to_string())?;
    if !status.is_success() {
        let message = serde_json::from_str::<ApiResult>(&body)
            .map(|result| result.message)
            .unwrap_or(body);
        return Err(message);
    }
    serde_json::from_str(&body).map_err(|error| format!("服务响应无效：{error}"))
}

fn fit_size(source: egui::Vec2, bounds: egui::Vec2) -> egui::Vec2 {
    let scale = (bounds.x / source.x).min(bounds.y / source.y).min(1.0);
    source * scale
}

fn format_time(seconds: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    if bytes as f64 >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.1} MiB", bytes as f64 / MIB)
    }
}

fn main() -> eframe::Result {
    let icon = image::load_from_memory(include_bytes!("../../assets/xscreen-icon.png"))
        .expect("bundled application icon must be a valid PNG")
        .resize_exact(256, 256, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("xscreen 投屏控制台")
            .with_app_id("xscreen")
            .with_icon(egui::IconData {
                rgba: icon.into_raw(),
                width: 256,
                height: 256,
            })
            .with_inner_size([960.0, 780.0])
            .with_min_inner_size([760.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "xscreen 投屏控制台",
        options,
        Box::new(|context| Ok(Box::new(XscreenGui::new(context)))),
    )
}
