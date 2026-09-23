const extensionApi = globalThis.browser ?? globalThis.chrome;
const MAX_CANDIDATES_PER_TAB = 100;
const MEDIA_URL_PATTERN = /\.(m3u8|mpd|mp4|m4v|webm|mkv|mov|mp3|m4a|aac|flac|m4s|ts)(?:$|[?#])/i;
const MANIFEST_URL_PATTERN = /\.(m3u8|mpd)(?:$|[?#])/i;
const SEGMENT_URL_PATTERN = /\.(m4s|ts)(?:$|[?#])/i;
const tabQueues = new Map();

function enqueue(tabId, operation) {
  const previous = tabQueues.get(tabId) || Promise.resolve();
  const next = previous.catch(() => {}).then(operation);
  tabQueues.set(tabId, next);
  void next.finally(() => {
    if (tabQueues.get(tabId) === next) {
      tabQueues.delete(tabId);
    }
  });
  return next;
}

function storageKey(tabId) {
  return `tab:${tabId}`;
}

function responseHeader(headers, name) {
  const wanted = name.toLowerCase();
  return (headers || []).find((header) => header.name.toLowerCase() === wanted)?.value || "";
}

function classify(url, contentType = "", requestType = "") {
  const mime = contentType.toLowerCase().split(";", 1)[0].trim();
  if (MANIFEST_URL_PATTERN.test(url) || mime.includes("mpegurl") || mime === "application/dash+xml") {
    return { kind: "manifest", score: 100 };
  }
  if (SEGMENT_URL_PATTERN.test(url) || mime.includes("iso.segment") || mime.includes("mp2t")) {
    return { kind: "segment", score: 10 };
  }
  if (mime.startsWith("video/") || mime.startsWith("audio/") || requestType === "media") {
    return { kind: "media", score: 80 };
  }
  if (MEDIA_URL_PATTERN.test(url)) {
    return { kind: "media", score: 60 };
  }
  return null;
}

async function readCandidates(tabId) {
  const key = storageKey(tabId);
  const stored = await extensionApi.storage.session.get(key);
  return Array.isArray(stored[key]) ? stored[key] : [];
}

async function writeCandidates(tabId, candidates) {
  const key = storageKey(tabId);
  await extensionApi.storage.session.set({ [key]: candidates.slice(0, MAX_CANDIDATES_PER_TAB) });
  const visibleCount = candidates.filter((candidate) => candidate.kind !== "segment").length;
  await extensionApi.action.setBadgeBackgroundColor({ tabId, color: "#2563eb" }).catch(() => {});
  await extensionApi.action.setBadgeText({
    tabId,
    text: visibleCount > 0 ? String(Math.min(visibleCount, 99)) : ""
  }).catch(() => {});
}

async function clearCandidates(tabId) {
  await extensionApi.storage.session.remove(storageKey(tabId));
  await extensionApi.action.setBadgeText({ tabId, text: "" }).catch(() => {});
}

async function recordCandidate(details, contentType = "", contentLength = "") {
  if (details.tabId < 0 || details.url.startsWith("blob:") || details.url.startsWith("data:")) {
    return;
  }
  const classification = classify(details.url, contentType, details.type);
  if (!classification) {
    return;
  }

  const candidates = await readCandidates(details.tabId);
  const existing = candidates.find((candidate) => candidate.url === details.url);
  if (existing) {
    existing.contentType = contentType || existing.contentType;
    existing.contentLength = contentLength || existing.contentLength;
    existing.kind = classification.kind;
    existing.score = Math.max(existing.score, classification.score);
    existing.lastSeen = Date.now();
  } else {
    candidates.push({
      url: details.url,
      kind: classification.kind,
      score: classification.score,
      contentType,
      contentLength,
      requestType: details.type,
      initiator: details.initiator || "",
      firstSeen: Date.now(),
      lastSeen: Date.now()
    });
  }

  candidates.sort((left, right) => right.score - left.score || right.lastSeen - left.lastSeen);
  await writeCandidates(details.tabId, candidates);
}

extensionApi.webRequest.onBeforeRequest.addListener(
  (details) => {
    if (details.type === "main_frame") {
      void enqueue(details.tabId, () => clearCandidates(details.tabId));
      return;
    }
    if (details.type === "media" || MEDIA_URL_PATTERN.test(details.url)) {
      void enqueue(details.tabId, () => recordCandidate(details));
    }
  },
  { urls: ["<all_urls>"] }
);

extensionApi.webRequest.onHeadersReceived.addListener(
  (details) => {
    const contentType = responseHeader(details.responseHeaders, "content-type");
    const contentLength = responseHeader(details.responseHeaders, "content-length");
    if (classify(details.url, contentType, details.type)) {
      void enqueue(details.tabId, () => recordCandidate(details, contentType, contentLength));
    }
  },
  { urls: ["<all_urls>"] },
  ["responseHeaders"]
);

extensionApi.tabs.onRemoved.addListener((tabId) => {
  void enqueue(tabId, () => extensionApi.storage.session.remove(storageKey(tabId)));
});

extensionApi.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.type === "get-candidates" && Number.isInteger(message.tabId)) {
    readCandidates(message.tabId)
      .then((candidates) => sendResponse({ ok: true, candidates }))
      .catch((error) => sendResponse({ ok: false, error: String(error) }));
    return true;
  }
  if (message?.type === "clear-candidates" && Number.isInteger(message.tabId)) {
    enqueue(message.tabId, () => clearCandidates(message.tabId))
      .then(() => sendResponse({ ok: true }))
      .catch((error) => sendResponse({ ok: false, error: String(error) }));
    return true;
  }
  return false;
});
