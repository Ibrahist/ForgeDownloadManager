// Forge Download Manager — Background Service Worker (Manifest V3)
import { browser } from "./shim.js";

const HOST_NAME = "com.forgedownloadmanager.host";

// Default settings
const DEFAULT_SETTINGS = {
  interceptionEnabled: true,
  altKeyBypass: true,
  minFileSizeKb: 0,
  fileExtensions: [
    "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "iso", "dmg", "pkg",
    "exe", "msi", "bin", "deb", "rpm", "apk",
    "mp4", "mkv", "avi", "webm", "mov", "flv", "m3u8", "ts",
    "mp3", "flac", "wav", "ogg", "m4a", "aac",
    "pdf", "epub", "docx", "xlsx", "pptx"
  ],
  siteRules: [],
  capturedCount: 0
};

// State caches
const detectedMediaByTab = new Map();
const recentAltKeyPress = new Map(); // tabId -> boolean
let cachedSettings = { ...DEFAULT_SETTINGS };

// Initialize settings and context menus on install/startup
browser.runtime.onInstalled.addListener(async () => {
  const stored = await browser.storage.local.get(null);
  cachedSettings = { ...DEFAULT_SETTINGS, ...stored };
  await browser.storage.local.set(cachedSettings);

  setupContextMenus();
});

browser.runtime.onStartup.addListener(async () => {
  const stored = await browser.storage.local.get(null);
  cachedSettings = { ...DEFAULT_SETTINGS, ...stored };
  setupContextMenus();
});

// Sync changes from storage
browser.storage.onChanged.addListener((changes) => {
  for (const [key, change] of Object.entries(changes)) {
    cachedSettings[key] = change.newValue;
  }
});

// Setup Context Menus
function setupContextMenus() {
  browser.contextMenus.removeAll(() => {
    browser.contextMenus.create({
      id: "forge-download-link",
      title: "Download with Forge",
      contexts: ["link"]
    });

    browser.contextMenus.create({
      id: "forge-download-media",
      title: "Download Media with Forge",
      contexts: ["video", "audio", "image"]
    });

    browser.contextMenus.create({
      id: "forge-download-page-links",
      title: "Download All Links on Page",
      contexts: ["page"]
    });
  });
}

// Handle Context Menu clicks
browser.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId === "forge-download-link" && info.linkUrl) {
    await sendUrlToForge(info.linkUrl, {
      referer: tab?.url,
      tabId: tab?.id
    });
  } else if (info.menuItemId === "forge-download-media" && info.srcUrl) {
    await sendUrlToForge(info.srcUrl, {
      referer: tab?.url,
      tabId: tab?.id
    });
  } else if (info.menuItemId === "forge-download-page-links" && tab?.id) {
    try {
      const response = await browser.tabs.sendMessage(tab.id, { type: "extract_links" });
      if (response && response.links && response.links.length > 0) {
        // Send batch or open grabber popup
        for (const link of response.links.slice(0, 50)) {
          await sendUrlToForge(link, { referer: tab.url });
        }
      }
    } catch (e) {
      console.warn("Could not extract links from tab:", e);
    }
  }
});

// Send message to native messaging host
export function sendNative(message) {
  return new Promise((resolve) => {
    try {
      browser.runtime.sendNativeMessage(HOST_NAME, message, (response) => {
        const err = browser.runtime.lastError;
        if (err) {
          resolve({ ok: false, error: err.message });
        } else {
          resolve({ ok: true, data: response });
        }
      });
    } catch (e) {
      resolve({ ok: false, error: e.message });
    }
  });
}

// Forward download to Forge
async function sendUrlToForge(url, options = {}) {
  // Capture cookies for domain
  let cookies = [];
  try {
    const rawCookies = await browser.cookies.getAll({ url });
    cookies = rawCookies.map(c => ({ name: c.name, value: c.value }));
  } catch (e) {
    console.warn("Cookie capture skipped:", e);
  }

  const payload = {
    type: "add",
    url,
    filename: options.filename,
    cookies,
    referer: options.referer,
    user_agent: navigator.userAgent
  };

  const res = await sendNative(payload);
  if (res.ok && res.data && res.data.success) {
    cachedSettings.capturedCount = (cachedSettings.capturedCount || 0) + 1;
    await browser.storage.local.set({ capturedCount: cachedSettings.capturedCount });
    updateBadge();
    return res.data;
  }
  return null;
}

function updateBadge() {
  const count = cachedSettings.capturedCount || 0;
  if (count > 0) {
    browser.action.setBadgeText({ text: String(count > 999 ? "999+" : count) });
    browser.action.setBadgeBackgroundColor({ color: "#f97316" }); // Forge Amber
  } else {
    browser.action.setBadgeText({ text: "" });
  }
}

// Helper: check if file extension matches list
function matchesExtension(filenameOrUrl) {
  if (!filenameOrUrl) return false;
  try {
    const clean = filenameOrUrl.split("?")[0].split("#")[0];
    const dot = clean.lastIndexOf(".");
    if (dot === -1 || dot === clean.length - 1) return false;
    const ext = clean.slice(dot + 1).toLowerCase();
    return cachedSettings.fileExtensions.includes(ext);
  } catch {
    return false;
  }
}

// Helper: check site rules (pattern glob matching)
function evaluateSiteRules(urlStr) {
  try {
    const parsed = new URL(urlStr);
    const host = parsed.hostname.toLowerCase();
    const rules = cachedSettings.siteRules || [];
    for (const rule of rules) {
      const pat = (rule.pattern || "").trim().toLowerCase();
      if (!pat) continue;
      if (pat === host) return rule.action;
      if (pat.startsWith("*.") && (host === pat.slice(2) || host.endsWith("." + pat.slice(2)))) {
        return rule.action;
      }
      if (pat.startsWith(".") && (host === pat.slice(1) || host.endsWith(pat))) {
        return rule.action;
      }
      if (pat.startsWith("*") && pat.endsWith("*") && pat.length > 2) {
        if (host.includes(pat.slice(1, -1))) return rule.action;
      }
    }
  } catch {}
  return null;
}

// Intercept browser downloads
browser.downloads.onCreated.addListener(async (item) => {
  if (!cachedSettings.interceptionEnabled) {
    return;
  }

  // Check Alt key bypass
  if (cachedSettings.altKeyBypass) {
    // If Alt key was recently held or tab signaled bypass
    // We check recentAltKeyPress map
  }

  const siteAction = evaluateSiteRules(item.url);
  if (siteAction === "exclude") {
    return;
  }

  const isMatchedExt = matchesExtension(item.filename || item.url);
  const isForcedIntercept = siteAction === "intercept";

  if (!isMatchedExt && !isForcedIntercept) {
    return;
  }

  // Check file size threshold if known
  if (cachedSettings.minFileSizeKb > 0 && item.fileSize > 0) {
    if (item.fileSize < cachedSettings.minFileSizeKb * 1024) {
      return;
    }
  }

  // Cancel the native browser download and delete from shelf
  try {
    await browser.downloads.cancel(item.id);
    await browser.downloads.erase({ id: item.id });
  } catch (e) {
    console.warn("Could not cancel browser download:", e);
  }

  // Forward to Forge Download Manager
  await sendUrlToForge(item.url, {
    filename: item.filename ? item.filename.split(/[\\/]/).pop() : undefined,
    referer: item.referrer
  });
});

// Runtime message listener for UI components
browser.runtime.onMessage.addListener((request, sender, sendResponse) => {
  if (request.type === "ping_host") {
    sendNative({ type: "ping" }).then(sendResponse);
    return true;
  }

  if (request.type === "get_status") {
    sendNative({ type: "status" }).then(sendResponse);
    return true;
  }

  if (request.type === "get_site_rules") {
    sendNative({ type: "site_rules" }).then(sendResponse);
    return true;
  }

  if (request.type === "add_download") {
    sendUrlToForge(request.url, {
      filename: request.filename,
      referer: request.referer
    }).then(res => sendResponse({ ok: !!res, data: res }));
    return true;
  }

  if (request.type === "media_detected" && sender.tab?.id) {
    const list = detectedMediaByTab.get(sender.tab.id) || [];
    if (!list.some(m => m.url === request.media.url)) {
      list.push(request.media);
      detectedMediaByTab.set(sender.tab.id, list);
    }
    sendResponse({ ok: true, count: list.length });
    return true;
  }

  if (request.type === "get_detected_media") {
    const list = detectedMediaByTab.get(request.tabId) || [];
    sendResponse({ media: list });
    return true;
  }

  if (request.type === "set_alt_key" && sender.tab?.id) {
    recentAltKeyPress.set(sender.tab.id, request.active);
    sendResponse({ ok: true });
    return true;
  }
});

// Clean up closed tabs from memory
browser.tabs.onRemoved.addListener((tabId) => {
  detectedMediaByTab.delete(tabId);
  recentAltKeyPress.delete(tabId);
});
