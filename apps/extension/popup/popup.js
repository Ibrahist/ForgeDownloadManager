// Forge Download Manager — Popup Script
import { browser } from "../background/shim.js";

document.addEventListener("DOMContentLoaded", async () => {
  // DOM Elements
  const connectionStatus = document.getElementById("connectionStatus");
  const interceptionToggle = document.getElementById("interceptionToggle");
  const altBypassToggle = document.getElementById("altBypassToggle");
  const tabBtns = document.querySelectorAll(".tab-btn");
  const tabContents = document.querySelectorAll(".tab-content");
  const quickUrlInput = document.getElementById("quickUrlInput");
  const quickAddBtn = document.getElementById("quickAddBtn");
  const statIntercepted = document.getElementById("statIntercepted");
  const statActive = document.getElementById("statActive");
  const statRules = document.getElementById("statRules");
  const mediaBadge = document.getElementById("mediaBadge");
  const mediaList = document.getElementById("mediaList");
  const refreshMediaBtn = document.getElementById("refreshMediaBtn");
  const scanLinksBtn = document.getElementById("scanLinksBtn");
  const linkFilterInput = document.getElementById("linkFilterInput");
  const linksList = document.getElementById("linksList");
  const downloadAllLinksBtn = document.getElementById("downloadAllLinksBtn");
  const selectedCount = document.getElementById("selectedCount");
  const openOptionsBtn = document.getElementById("openOptionsBtn");
  const quickNotice = document.getElementById("quickNotice");

  let activeTab = null;
  let detectedMedia = [];
  let extractedLinks = [];
  let selectedLinks = new Set();

  // Get active tab
  try {
    const tabs = await browser.tabs.query({ active: true, currentWindow: true });
    activeTab = tabs[0];
  } catch {}

  // Load Settings
  const settings = await browser.storage.local.get([
    "interceptionEnabled",
    "altKeyBypass",
    "capturedCount",
    "siteRules"
  ]);

  interceptionToggle.checked = settings.interceptionEnabled !== false;
  altBypassToggle.checked = settings.altKeyBypass !== false;
  statIntercepted.textContent = String(settings.capturedCount || 0);
  statRules.textContent = String((settings.siteRules || []).length);

  // Settings change handlers
  interceptionToggle.addEventListener("change", () => {
    browser.storage.local.set({ interceptionEnabled: interceptionToggle.checked });
    showNotice(interceptionToggle.checked ? "Interception enabled" : "Interception paused");
  });

  altBypassToggle.addEventListener("change", () => {
    browser.storage.local.set({ altKeyBypass: altBypassToggle.checked });
  });

  // Tab navigation
  tabBtns.forEach(btn => {
    btn.addEventListener("click", () => {
      tabBtns.forEach(b => b.classList.remove("active"));
      tabContents.forEach(c => c.classList.remove("active"));
      btn.classList.add("active");
      const target = document.getElementById(btn.dataset.tab);
      if (target) target.classList.add("active");
    });
  });

  // Check Host Connection
  async function checkHost() {
    try {
      const res = await browser.runtime.sendMessage({ type: "ping_host" });
      if (res && res.ok && res.data) {
        if (res.data.running) {
          connectionStatus.className = "status-pill status-connected";
          connectionStatus.querySelector(".status-label").textContent = "Connected";
        } else {
          connectionStatus.className = "status-pill status-disconnected";
          connectionStatus.querySelector(".status-label").textContent = "Offline Queue";
        }
      } else {
        connectionStatus.className = "status-pill status-disconnected";
        connectionStatus.querySelector(".status-label").textContent = "Standby";
      }
    } catch {
      connectionStatus.className = "status-pill status-disconnected";
      connectionStatus.querySelector(".status-label").textContent = "Standby";
    }

    // Also fetch status to see active jobs
    try {
      const statusRes = await browser.runtime.sendMessage({ type: "get_status" });
      if (statusRes && statusRes.ok && statusRes.data) {
        const dls = Array.isArray(statusRes.data.downloads) ? statusRes.data.downloads : [];
        const active = dls.filter(d => d.status === "downloading" || d.status === "probing").length;
        statActive.textContent = String(active);
      }
    } catch {}
  }
  checkHost();

  // Quick Add Download
  quickAddBtn.addEventListener("click", async () => {
    const url = quickUrlInput.value.trim();
    if (!url) return;

    quickAddBtn.disabled = true;
    try {
      const res = await browser.runtime.sendMessage({
        type: "add_download",
        url,
        referer: activeTab?.url
      });
      if (res && res.ok) {
        showNotice("Added to Forge!");
        quickUrlInput.value = "";
        checkHost();
      } else {
        showNotice("Could not add download");
      }
    } catch (e) {
      showNotice("Error sending to Forge");
    } finally {
      quickAddBtn.disabled = false;
    }
  });

  quickUrlInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") quickAddBtn.click();
  });

  // Media Tab
  async function loadMedia() {
    if (!activeTab?.id) return;
    try {
      const res = await browser.runtime.sendMessage({
        type: "get_detected_media",
        tabId: activeTab.id
      });
      detectedMedia = res?.media || [];
      mediaBadge.textContent = String(detectedMedia.length);

      if (detectedMedia.length === 0) {
        mediaList.innerHTML = `<div class="empty-state">No audio or video streams detected on this tab.</div>`;
        return;
      }

      mediaList.innerHTML = "";
      detectedMedia.forEach((m, idx) => {
        const item = document.createElement("div");
        item.className = "list-item";
        item.innerHTML = `
          <div>
            <div class="list-item-title">${escapeHtml(m.title || "Media " + (idx + 1))}</div>
            <div class="list-item-sub">${m.type.toUpperCase()}${m.duration ? " • " + formatDuration(m.duration) : ""}</div>
          </div>
          <button class="small-btn dl-btn">Download</button>
        `;
        item.querySelector(".dl-btn").addEventListener("click", async (e) => {
          const btn = e.target;
          btn.disabled = true;
          btn.textContent = "Sending...";
          await browser.runtime.sendMessage({
            type: "add_download",
            url: m.url,
            referer: activeTab?.url
          });
          btn.textContent = "Added!";
          setTimeout(() => {
            btn.disabled = false;
            btn.textContent = "Download";
          }, 2000);
        });
        mediaList.appendChild(item);
      });
    } catch {}
  }

  refreshMediaBtn.addEventListener("click", loadMedia);
  loadMedia();

  // Page Links Tab
  scanLinksBtn.addEventListener("click", async () => {
    if (!activeTab?.id) return;
    scanLinksBtn.disabled = true;
    scanLinksBtn.textContent = "Scanning...";

    try {
      const res = await browser.tabs.sendMessage(activeTab.id, { type: "extract_links" });
      extractedLinks = res?.links || [];
      selectedLinks = new Set(extractedLinks);
      renderLinksList();
    } catch (e) {
      linksList.innerHTML = `<div class="empty-state">Could not read links from this page.</div>`;
    } finally {
      scanLinksBtn.disabled = false;
      scanLinksBtn.textContent = "Scan Tab";
    }
  });

  linkFilterInput.addEventListener("input", renderLinksList);

  function renderLinksList() {
    const filter = linkFilterInput.value.trim().toLowerCase();
    const filtered = extractedLinks.filter(url => {
      if (!filter) return true;
      return url.toLowerCase().includes(filter);
    });

    if (filtered.length === 0) {
      linksList.innerHTML = `<div class="empty-state">No matching links found.</div>`;
      downloadAllLinksBtn.disabled = true;
      selectedCount.textContent = "0";
      return;
    }

    linksList.innerHTML = "";
    filtered.forEach(url => {
      const filename = url.split("/").pop().split("?")[0] || url;
      const isChecked = selectedLinks.has(url);
      const row = document.createElement("div");
      row.className = "list-item";
      row.innerHTML = `
        <label style="display:flex;align-items:center;gap:8px;cursor:pointer;flex:1;overflow:hidden;">
          <input type="checkbox" class="link-chk" ${isChecked ? "checked" : ""}>
          <div>
            <div class="list-item-title">${escapeHtml(filename)}</div>
            <div class="list-item-sub">${escapeHtml(url.slice(0, 45))}...</div>
          </div>
        </label>
      `;

      row.querySelector(".link-chk").addEventListener("change", (e) => {
        if (e.target.checked) selectedLinks.add(url);
        else selectedLinks.delete(url);
        updateSelectedLinksCount();
      });

      linksList.appendChild(row);
    });

    updateSelectedLinksCount();
  }

  function updateSelectedLinksCount() {
    selectedCount.textContent = String(selectedLinks.size);
    downloadAllLinksBtn.disabled = selectedLinks.size === 0;
  }

  downloadAllLinksBtn.addEventListener("click", async () => {
    if (selectedLinks.size === 0) return;
    downloadAllLinksBtn.disabled = true;
    downloadAllLinksBtn.textContent = "Sending batch...";

    for (const url of selectedLinks) {
      await browser.runtime.sendMessage({
        type: "add_download",
        url,
        referer: activeTab?.url
      });
    }

    showNotice(`Sent ${selectedLinks.size} links to Forge!`);
    downloadAllLinksBtn.textContent = "Batch Added!";
    setTimeout(() => {
      downloadAllLinksBtn.disabled = false;
      downloadAllLinksBtn.textContent = `Download Selected (${selectedLinks.size})`;
    }, 2500);
  });

  // Open Options Page
  openOptionsBtn.addEventListener("click", () => {
    if (browser.runtime.openOptionsPage) {
      browser.runtime.openOptionsPage();
    } else {
      window.open(browser.runtime.getURL("options/options.html"));
    }
  });

  // Helpers
  function showNotice(text) {
    quickNotice.textContent = text;
    quickNotice.classList.add("show");
    setTimeout(() => quickNotice.classList.remove("show"), 2500);
  }

  function formatDuration(sec) {
    const m = Math.floor(sec / 60);
    const s = sec % 60;
    return `${m}:${s < 10 ? "0" : ""}${s}`;
  }

  function escapeHtml(str) {
    return String(str)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }
});
