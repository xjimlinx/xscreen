const results = document.querySelector("#results");
const status = document.querySelector("#status");
const deviceInput = document.querySelector("#device");
const tokenInput = document.querySelector("#token");
const deviceOptions = document.querySelector("#device-options");
const showSegmentsInput = document.querySelector("#show-segments");
const template = document.querySelector("#candidate-template");
const BRIDGE_URL = "http://127.0.0.1:47821";

let activeTabId = null;
let candidates = [];

function shellQuote(value) {
  return `'${String(value).replaceAll("'", `'"'"'`)}'`;
}

function formatBytes(value) {
  const bytes = Number(value);
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "";
  }
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KiB`;
  }
  return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
}

async function copyText(value, message) {
  await navigator.clipboard.writeText(value);
  status.textContent = message;
  window.setTimeout(() => renderStatus(), 1800);
}

function castCommand(candidate) {
  const device = deviceInput.value.trim() || "1";
  return `xscreen cast ${shellQuote(candidate.url)} --device ${shellQuote(device)}`;
}

async function bridgeRequest(path, options = {}) {
  const token = tokenInput.value.trim();
  if (!token) {
    throw new Error("请先启动 xscreen bridge 并填写配对令牌");
  }
  const response = await fetch(`${BRIDGE_URL}${path}`, {
    ...options,
    headers: {
      "Authorization": `Bearer ${token}`,
      ...(options.body ? { "Content-Type": "application/json" } : {}),
      ...(options.headers || {})
    }
  });
  const data = await response.json().catch(() => ({ message: `HTTP ${response.status}` }));
  if (!response.ok) {
    throw new Error(data.message || `HTTP ${response.status}`);
  }
  return data;
}

async function refreshDevices() {
  if (!tokenInput.value.trim()) {
    deviceOptions.replaceChildren();
    return;
  }
  status.textContent = "正在扫描电视…";
  const devices = await bridgeRequest("/api/devices");
  deviceOptions.replaceChildren();
  for (const device of devices) {
    const option = document.createElement("option");
    option.value = device.friendlyName;
    option.label = `${device.index}. ${device.modelName || "DLNA"} (${device.address})`;
    deviceOptions.append(option);
  }
  if (devices.length === 1 && (deviceInput.value.trim() === "" || deviceInput.value === "1")) {
    deviceInput.value = devices[0].friendlyName;
  }
}

async function castWithXscreen(candidate) {
  status.textContent = "正在通过 xscreen 投送…";
  const result = await bridgeRequest("/api/cast", {
    method: "POST",
    body: JSON.stringify({
      url: candidate.url,
      device: deviceInput.value.trim() || "1"
    })
  });
  status.textContent = result.message || "已发送到电视";
}

function renderStatus() {
  const visible = candidates.filter((candidate) => showSegmentsInput.checked || candidate.kind !== "segment");
  status.textContent = visible.length > 0 ? `检测到 ${visible.length} 个候选地址` : "等待媒体请求";
}

function render() {
  const visible = candidates.filter((candidate) => showSegmentsInput.checked || candidate.kind !== "segment");
  results.replaceChildren();

  if (visible.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = candidates.length > 0
      ? "只检测到媒体分片。勾选“显示媒体分片”查看。"
      : "请播放网页视频，检测到的媒体会显示在这里。";
    results.append(empty);
    renderStatus();
    return;
  }

  for (const candidate of visible) {
    const fragment = template.content.cloneNode(true);
    const article = fragment.querySelector(".candidate");
    const kind = fragment.querySelector(".kind");
    kind.textContent = candidate.kind;
    kind.classList.toggle("segment", candidate.kind === "segment");
    fragment.querySelector(".mime").textContent = candidate.contentType || candidate.requestType || "unknown";
    const urlElement = fragment.querySelector(".url");
    urlElement.textContent = candidate.url;
    urlElement.title = candidate.url;

    const details = [];
    const size = formatBytes(candidate.contentLength);
    if (size) {
      details.push(size);
    }
    try {
      details.push(new URL(candidate.url).hostname);
    } catch {
      // Keep malformed URLs visible without failing the popup.
    }
    fragment.querySelector(".meta").textContent = details.join(" · ");
    fragment.querySelector(".copy-url").addEventListener("click", () => {
      void copyText(candidate.url, "URL 已复制");
    });
    fragment.querySelector(".copy-command").addEventListener("click", () => {
      void copyText(castCommand(candidate), "投屏命令已复制");
    });
    fragment.querySelector(".cast").addEventListener("click", () => {
      void castWithXscreen(candidate).catch((error) => {
        status.textContent = error.message;
      });
    });
    results.append(article);
  }
  renderStatus();
}

async function refresh() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  activeTabId = tab?.id ?? null;
  if (activeTabId === null) {
    candidates = [];
    render();
    status.textContent = "没有活动标签页";
    return;
  }
  const response = await chrome.runtime.sendMessage({ type: "get-candidates", tabId: activeTabId });
  candidates = response?.ok && Array.isArray(response.candidates) ? response.candidates : [];
  render();
}

document.querySelector("#refresh").addEventListener("click", () => {
  void Promise.all([refresh(), refreshDevices()]).catch((error) => {
    status.textContent = error.message;
  });
});
document.querySelector("#clear").addEventListener("click", async () => {
  if (activeTabId !== null) {
    await chrome.runtime.sendMessage({ type: "clear-candidates", tabId: activeTabId });
  }
  candidates = [];
  render();
});
showSegmentsInput.addEventListener("change", render);
deviceInput.addEventListener("change", () => {
  void chrome.storage.local.set({ device: deviceInput.value.trim() });
});
tokenInput.addEventListener("change", () => {
  void chrome.storage.local.set({ bridgeToken: tokenInput.value.trim() });
  void refreshDevices().catch((error) => {
    status.textContent = error.message;
  });
});

async function initialize() {
  const { device, bridgeToken } = await chrome.storage.local.get(["device", "bridgeToken"]);
  if (typeof device === "string" && device) {
    deviceInput.value = device;
  }
  if (typeof bridgeToken === "string" && bridgeToken) {
    tokenInput.value = bridgeToken;
  }
  await refresh();
  if (tokenInput.value) {
    await refreshDevices();
    renderStatus();
  }
}

void initialize().catch((error) => {
  status.textContent = error.message;
});
