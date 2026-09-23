# xscreen 中文使用手册

## 1. xscreen 是什么

`xscreen` 是一个 Rust 编写的 DLNA/UPnP 媒体投送工具。电脑负责发现电视、发送播放控制命令，并在投送本地文件时提供临时 HTTP 文件服务；电视负责下载和解码媒体。

它适合以下场景：

- 把电脑上的 MP4、TS、MKV、MP3 或图片交给智能电视播放。
- 把电视能够直接访问的 HTTP(S)、HLS 或 DASH 地址发送给电视。
- 使用 Chrome 扩展探测网页正常加载时产生的媒体地址，再交给 `xscreen` 投送。

它不是桌面镜像工具。需要显示整个桌面、浏览器界面或 DRM 视频时，应使用 Miracast、Chromecast 屏幕共享或 HDMI。

## 2. 工作方式

投送本地文件：

```text
本地文件 → xscreen 临时 HTTP 服务 → 局域网 → 电视解码播放
```

投送网络地址：

```text
xscreen 发送 URL → 电视直接连接媒体服务器 → 电视解码播放
```

因此，本地文件播放期间不能关闭 `xscreen`。网络 URL 成功加载后，电视通常可以独立继续播放。

## 3. 网络要求

- 电脑和电视必须位于可互相访问的局域网。
- 电脑接网线、电视接 Wi-Fi 也可以，只要属于同一路由网络。
- 电视需要开启“DLNA”“媒体共享”“多屏互动”或类似功能。
- 路由器不能开启 AP 隔离或客户端隔离。
- 酒店、校园及访客 Wi-Fi 经常会隔离不同终端。
- 防火墙需要允许 SSDP UDP 组播，并允许电视访问 `xscreen` 的临时 TCP 端口。

## 4. 构建

进入项目目录：

```bash
cd xscreen
cargo build --release
```

如需中文图形界面，额外构建：

```bash
cargo build --release --features gui --bin xscreen-gui
```

生成的程序为：

```text
target/release/xscreen
target/release/xscreen-gui
```

可选地安装到用户命令目录：

```bash
install -Dm755 target/release/xscreen ~/.local/bin/xscreen
install -Dm755 target/release/xscreen-gui ~/.local/bin/xscreen-gui
```

确认 `~/.local/bin` 已加入 `PATH`。

## 5. 扫描电视

```bash
target/release/xscreen scan
```

延长扫描时间：

```bash
target/release/xscreen scan --timeout 10
```

示例结果：

```text
1. 智能电视投屏(8436) — Microsoft Corporation Windows Media Player (192.168.43.165)
```

后续可以使用编号、名称或 IP 选择电视。

## 6. 投送本地文件

```bash
target/release/xscreen cast '/path/to/movie.mp4' --device 1
```

文件名包含空格时必须使用引号：

```bash
target/release/xscreen cast \
  '/path/to/2026_09_22 23_40_35.ts' \
  --device 1
```

也可以按名称选择：

```bash
target/release/xscreen cast './movie.mp4' --device '智能电视投屏'
```

固定本地媒体服务端口：

```bash
target/release/xscreen cast './movie.mp4' --device 1 --port 8899
```

播放期间需保持终端程序运行。

## 7. 投送网络媒体

公开 MP4：

```bash
target/release/xscreen cast \
  'https://example.com/movie.mp4' \
  --device 1
```

HLS：

```bash
target/release/xscreen cast \
  'https://example.com/master.m3u8' \
  --device 1
```

网络媒体由电视直接访问。以下地址通常不能直接投送：

- `blob:` 地址。
- 依赖登录 Cookie、Referer、Authorization 或其他特殊请求头的地址。
- 仅在短时间内有效的签名地址。
- 使用 Widevine 等 DRM 的媒体。
- 电视不支持的编码、封装或清单格式。

## 8. 播放控制

投送成功后会出现 `xscreen>` 提示符。

播放与暂停：

```text
play
pause
stop
```

尝试调整播放倍速：

```text
rate 1.5
rate 2
rate 0.5
```

`xscreen` 会把小数转换成 DLNA 规范中的有理数，例如 `1.5` 转成 `3/2`。DLNA 只要求电视支持正常速度 `1`，其他速度均为电视厂商的可选能力；不支持时会显示电视返回的错误。

跳转到第 90 秒：

```text
seek 90
```

按时分秒跳转：

```text
seek 01:20:30
```

查看播放进度：

```text
position
```

调整音量：

```text
volume 35
```

退出并停止电视播放：

```text
quit
```

命令是否生效取决于电视实现的 `AVTransport` 和 `RenderingControl` 能力。TS 文件跳转通常只能落在附近的关键帧，MP4 的兼容性一般更好。

## 9. 查看实际传输速度

投送本地文件时输入：

```text
stats
```

或：

```text
speed
```

示例：

```text
sent 128.40 MiB / 1.68 GiB in 52.1s, average 20.67 Mbps
```

含义：

- `sent`：电视已经从 `xscreen` 读取的累计数据量。
- 第二个数值：原始文件大小。
- `average`：从首次发送数据到现在的平均 HTTP 传输速率。

注意事项：

- 拖动进度会重新读取其他范围，累计发送量可能超过文件大小。
- 暂停或电视停止读取后，平均速率会逐渐下降。
- 对公网 URL，流量不经过 `xscreen`，因此无法由 `xscreen` 测速。
- 稳定播放通常希望实际传输能力至少达到媒体平均码率的 2～3 倍。

## 10. 中文图形界面

图形界面通过本机 bridge 服务连接现有的投屏功能。先在一个终端启动 bridge：

```bash
target/release/xscreen bridge
```

保留终端中显示的服务地址和配对令牌，不要关闭该进程。再打开另一个终端：

```bash
target/release/xscreen-gui
```

操作顺序：

1. 服务地址默认保持 `http://127.0.0.1:47821`。
2. 粘贴 bridge 输出的配对令牌。
3. 点击“扫描电视”并选择目标设备。
4. 点击“选择文件”，或输入电视可直接访问的 HTTP(S) 媒体地址。
5. 点击“预览”查看视频第 3 秒附近的画面。
6. 点击“投送到电视”。
7. 投送成功后使用进度条、播放、暂停、停止、倍速和音量控件。

预览依赖 `ffmpeg`，可用以下命令确认：

```bash
ffmpeg -version
```

没有安装 `ffmpeg` 时只影响预览图，不影响投送。GUI 会每秒向电视查询一次进度；拖动进度条后，松开鼠标才发送跳转命令。倍速、跳转与音量仍取决于电视是否实现对应的 DLNA 能力。

界面会优先加载 Noto Sans CJK、微软雅黑或霞鹜文楷等中文字体。如果中文显示为方框，请安装 Noto CJK 字体后重新启动：

```bash
# Arch Linux
sudo pacman -S noto-fonts-cjk
```

本地文件的实时传输量和平均速率会显示在音量条右侧；网络 URL 由电视直接读取，因此不显示传输统计。

## 11. Chrome 媒体探测扩展

扩展源码位于项目的 `extension` 目录，是标准 Chrome Manifest V3 扩展。

先启动本机桥接服务：

```bash
target/release/xscreen bridge
```

输出示例：

```text
xscreen browser bridge: http://127.0.0.1:47821
pairing token: 0123456789abcdef...
```

安装扩展：

1. 打开 `chrome://extensions`。
2. 开启“开发者模式”。
3. 点击“加载已解压的扩展程序”。
4. 选择项目中的 `extension` 目录。
5. 将桥接服务显示的配对令牌粘贴到扩展中。
6. 打开网页并开始播放视频。
7. 点击扩展图标。
8. 优先选择标记为 `manifest` 或 `media` 的候选地址。
9. 选择电视并点击“投送到电视”。

扩展只负责观察媒体请求和提供界面。电视发现、DLNA 控制和投送仍由本机 `xscreen` 完成。

桥接服务只监听 `127.0.0.1`，外部设备无法直接连接。每次启动默认生成新的随机令牌；需要固定令牌时：

```bash
target/release/xscreen bridge \
  --token '请换成自己的长随机字符串'
```

## 12. 媒体兼容性

兼容性主要由电视决定。优先选择：

- 容器：MP4。
- 视频：H.264 High/Main。
- 像素格式：YUV 4:2:0。
- 音频：AAC-LC 双声道。

TS 文件无法播放时，可以无损封装为 MP4：

```bash
ffmpeg -fflags +genpts \
  -i 'input.ts' \
  -map 0:v:0 -map 0:a:0 \
  -c copy -movflags +faststart \
  'output.mp4'
```

该操作不重新编码，不改变画质，通常比完整转码快得多。

需要控制视频码率时必须重新编码，例如：

```bash
ffmpeg -i 'input.ts' \
  -c:v libx264 -preset medium \
  -b:v 1500k -maxrate 1800k -bufsize 3000k \
  -c:a aac -b:a 128k \
  -movflags +faststart \
  'output.mp4'
```

当前 `xscreen` 不会自动转码。

## 13. 常见问题

### 扫描不到电视

1. 确认电视与电脑在同一可互通网络。
2. 确认电视开启 DLNA/媒体共享。
3. 避免访客、酒店或启用客户端隔离的 Wi-Fi。
4. 尝试 `scan --timeout 10`。
5. 检查电脑防火墙。

### 可以扫描但不能播放

1. 先使用 H.264 + AAC 的 MP4 测试。
2. 确认电视能够访问电脑显示的局域网媒体 URL。
3. 为防火墙开放 `--port` 指定的端口。
4. 将 TS/MKV 无损封装或转码为 MP4。

### 不能调进度

- 某些电视不实现 DLNA Seek。
- TS 的时间戳或关键帧索引可能不完整。
- 尝试无损封装为 MP4。

### 不能调音量

电视可能没有公开 DLNA `RenderingControl` 服务。此时使用电视遥控器控制音量。

### 关闭程序后停止播放

本地文件由 `xscreen` 提供服务，进程结束后电视无法继续读取。公网 URL 通常由电视直接读取，投送成功后可以独立播放。

## 14. 安全说明

- 本地媒体 URL 使用随机令牌，程序退出后立即失效。
- 浏览器桥接服务仅允许监听回环地址，并要求 Bearer 配对令牌。
- 扩展不读取或保存 Cookie、Authorization 等登录凭据。
- 候选媒体 URL 可能包含临时签名，只保存在浏览器会话存储中。
- 请只投送自己有权访问和播放的内容。
