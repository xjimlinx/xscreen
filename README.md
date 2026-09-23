# xscreen

`xscreen` 是一个用 Rust 编写的轻量级 DLNA/UPnP 投屏工具。它能在局域网中发现智能电视，将本地媒体文件通过临时 HTTP 服务交给电视播放，也可以投送电视可直接访问的 HTTP(S) 媒体地址。

> 当前版本做的是“媒体投送”，不是电脑桌面镜像。电视直接读取并解码媒体，因此支持的封装、编码和字幕格式取决于电视。

## 功能

- 通过 SSDP 发现 DLNA `MediaRenderer`
- 解析 UPnP 设备描述及内嵌设备服务
- AVTransport：加载、播放、暂停、停止、跳转和进度查询
- RenderingControl：音量控制（电视支持时）
- 为本地文件提供带随机访问令牌的 HTTP 服务
- 支持 HTTP `Range`、`HEAD` 及 DLNA 响应头
- 可选中文 GUI：设备扫描、文件选择、画面预览、进度拖动、倍速和音量
- 支持交互选择电视，也支持脚本通过名称、IP 或编号选择

## 构建

需要稳定版 Rust 工具链：

```bash
cargo build --release
```

生成的程序位于 `target/release/xscreen`。

构建图形界面版本：

```bash
cargo build --release --features gui --bin xscreen-gui
```

生成的图形程序位于 `target/release/xscreen-gui`。预览画面需要系统安装 `ffmpeg`；不安装仍可投送和控制。

GUI 图标的矢量原稿见 [`assets/xscreen-icon.svg`](assets/xscreen-icon.svg)，PNG 版本用于窗口和桌面菜单。

安装到当前用户的命令目录并添加桌面菜单入口：

```bash
install -Dm755 target/release/xscreen ~/.local/bin/xscreen
install -Dm755 target/release/xscreen-gui ~/.local/bin/xscreen-gui
install -Dm644 assets/xscreen-icon.png ~/.local/share/pixmaps/xscreen.png
install -Dm644 packaging/xscreen.desktop ~/.local/share/applications/xscreen.desktop
```

## 使用

扫描电视：

```bash
cargo run -- scan
```

投送本地视频：

```bash
cargo run -- cast ./movie.mp4
```

指定电视或固定媒体服务端口：

```bash
cargo run -- cast ./movie.mp4 --device "Living Room" --port 8899
```

投送公开媒体 URL：

```bash
cargo run -- cast 'https://example.com/video.mp4' --device 1
```

## 中文图形界面

GUI 连接本机 bridge 服务，两个程序需要同时运行。先在第一个终端启动服务：

```bash
target/release/xscreen bridge
```

复制它输出的配对令牌，再在第二个终端启动 GUI：

```bash
target/release/xscreen-gui
```

在界面中粘贴令牌，依次点击“扫描电视”、选择文件或填写媒体 URL、“投送到电视”。投送后可拖动进度条、播放/暂停/停止、调整音量和尝试倍速播放。本地文件投送期间必须保持 bridge 运行；倍速、跳转和音量是否可用取决于电视的 DLNA 实现。

## 浏览器媒体探测扩展（Chrome / Firefox）

项目附带一个 Chrome 和 Firefox 共用的 Manifest V3 扩展，位于 [`extension/`](extension/)。它会观察当前标签页正常产生的网络请求，自动识别 MP4、HLS、DASH 和媒体分片。扩展只负责媒体探测和界面，设备发现及 DLNA 投送仍由本机的 Rust `xscreen` 完成。支持 Chrome 121+、Firefox 142+。

先启动本机桥接服务：

```bash
target/release/xscreen bridge
```

程序会输出一个随机配对令牌。桥接服务仅监听 `127.0.0.1:47821`，保持该进程运行。如果需要固定令牌，可使用 `--token`：

```bash
target/release/xscreen bridge --token '请换成自己的长随机字符串'
```

Chrome 安装方法：

1. 在 Chromium/Chrome 打开 `chrome://extensions`。
2. 开启“开发者模式”。
3. 点击“加载已解压的扩展程序”，选择本项目的 `extension` 目录。
4. 将 `xscreen bridge` 输出的配对令牌粘贴到扩展中。
5. 打开目标网页并开始播放视频。
6. 点击工具栏中的 xscreen 扩展图标，优先选择标记为 `manifest` 或 `media` 的地址。
7. 选择电视后点击“投送到电视”。也可以只复制 URL 或终端命令。

如果此前已加载过 Chrome 版，更新代码后到 `chrome://extensions` 点击该扩展的“重新加载”。

Firefox 临时安装方法：

1. 打开 `about:debugging`，选择“此 Firefox”（This Firefox）。
2. 点击“临时载入附加组件”（Load Temporary Add-on），选择项目的 `extension/manifest.json`。
3. 启动 `xscreen bridge`，把它输出的配对令牌粘贴到扩展弹窗中。
4. 打开视频网页并开始播放，再从扩展候选列表投送。

Firefox 的临时安装会在浏览器重启后失效；要长期安装，需要经 Mozilla 签名的 XPI。本项目目前提供开发安装用的源码，尚未发布签名版。

扩展需要读取所有站点的请求 URL，原因是媒体常由第三方 CDN 提供。候选地址只保存在浏览器会话存储中；只有用户点击“投送到电视”时，所选媒体 URL 才会发送到本机 `xscreen bridge`。扩展不会读取或保存 Cookie、Authorization 等登录凭据，也不会解密 DRM 内容。配对令牌保存在扩展自己的本地存储中。只有电视能够直接访问且支持解码的媒体地址才能通过 DLNA 播放，带临时签名的地址可能很快失效。

连接后可用命令：

```text
play
pause
stop
rate 1.5
seek 90
seek 00:10:30
volume 35
position
stats
quit
```

完整说明见 [中文使用手册](docs/USER_GUIDE.zh-CN.md)。

## 网络要求与排障

- 电脑和电视必须位于同一可互通的局域网。
- 电视设置中可能需要开启“DLNA”“媒体共享”或“多屏互动”。
- 路由器不能启用 AP/客户端隔离；访客网络通常无法发现设备。
- 防火墙需要允许程序发送 UDP 组播，并允许电视访问本机临时 TCP 端口。
- 如果能发现电视但不能播放，先尝试电视普遍支持的 H.264 + AAC MP4 文件。
- 在线地址必须能由电视直接访问；需要 Cookie、登录、特殊请求头或 DRM 的网页视频不能直接投送。

## 当前边界

- 仅实现 IPv4 SSDP/DLNA。
- 每次命令投送一个媒体源。
- 暂无自动转码、外挂字幕、播放列表、Google Cast、AirPlay 和屏幕镜像。
- 不同电视的 UPnP 实现存在差异，后续会根据真实设备兼容性补充厂商适配。
