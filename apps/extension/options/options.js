// Forge Download Manager — Options Script
import { browser } from "../background/shim.js";

document.addEventListener("DOMContentLoaded", async () => {
  const optInterception = document.getElementById("optInterception");
  const optAltBypass = document.getElementById("optAltBypass");
  const optMinSize = document.getElementById("optMinSize");
  const optExtensions = document.getElementById("optExtensions");
  const saveFeedback = document.getElementById("saveFeedback");

  const presetDefault = document.getElementById("presetDefault");
  const presetMedia = document.getElementById("presetMedia");
  const presetArchives = document.getElementById("presetArchives");
  const presetAll = document.getElementById("presetAll");

  const newRulePattern = document.getElementById("newRulePattern");
  const newRuleAction = document.getElementById("newRuleAction");
  const addRuleBtn = document.getElementById("addRuleBtn");
  const rulesTableBody = document.getElementById("rulesTableBody");

  const diagStatus = document.getElementById("diagStatus");
  const diagPort = document.getElementById("diagPort");
  const diagVersion = document.getElementById("diagVersion");
  const testHostBtn = document.getElementById("testHostBtn");

  const PRESETS = {
    default: [
      "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "iso", "dmg", "pkg",
      "exe", "msi", "bin", "deb", "rpm", "apk",
      "mp4", "mkv", "avi", "webm", "mov", "flv", "m3u8", "ts",
      "mp3", "flac", "wav", "ogg", "m4a", "aac",
      "pdf", "epub", "docx", "xlsx", "pptx"
    ],
    media: [
      "mp4", "mkv", "avi", "webm", "mov", "flv", "m3u8", "ts",
      "mp3", "flac", "wav", "ogg", "m4a", "aac"
    ],
    archives: [
      "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "iso", "dmg", "pkg"
    ],
    all: [
      "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "iso", "dmg", "pkg",
      "exe", "msi", "bin", "deb", "rpm", "apk",
      "mp4", "mkv", "avi", "webm", "mov", "flv", "m3u8", "ts",
      "mp3", "flac", "wav", "ogg", "m4a", "aac",
      "pdf", "epub", "docx", "xlsx", "pptx", "csv", "torrent"
    ]
  };

  // Load stored settings
  const settings = await browser.storage.local.get([
    "interceptionEnabled",
    "altKeyBypass",
    "minFileSizeKb",
    "fileExtensions",
    "siteRules"
  ]);

  optInterception.checked = settings.interceptionEnabled !== false;
  optAltBypass.checked = settings.altKeyBypass !== false;
  optMinSize.value = settings.minFileSizeKb || 0;
  optExtensions.value = (settings.fileExtensions || PRESETS.default).join(", ");

  let siteRules = settings.siteRules || [];
  renderRulesTable();

  // Save changes automatically
  function showSaved() {
    saveFeedback.classList.add("show");
    setTimeout(() => saveFeedback.classList.remove("show"), 2000);
  }

  optInterception.addEventListener("change", () => {
    browser.storage.local.set({ interceptionEnabled: optInterception.checked });
    showSaved();
  });

  optAltBypass.addEventListener("change", () => {
    browser.storage.local.set({ altKeyBypass: optAltBypass.checked });
    showSaved();
  });

  optMinSize.addEventListener("change", () => {
    const val = Math.max(0, parseInt(optMinSize.value, 10) || 0);
    optMinSize.value = val;
    browser.storage.local.set({ minFileSizeKb: val });
    showSaved();
  });

  optExtensions.addEventListener("change", () => {
    const raw = optExtensions.value;
    const list = raw
      .split(/[\s,]+/)
      .map(s => s.trim().replace(/^\./, "").toLowerCase())
      .filter(Boolean);
    const unique = Array.from(new Set(list));
    optExtensions.value = unique.join(", ");
    browser.storage.local.set({ fileExtensions: unique });
    showSaved();
  });

  // Presets
  presetDefault.addEventListener("click", () => applyPreset(PRESETS.default));
  presetMedia.addEventListener("click", () => applyPreset(PRESETS.media));
  presetArchives.addEventListener("click", () => applyPreset(PRESETS.archives));
  presetAll.addEventListener("click", () => applyPreset(PRESETS.all));

  function applyPreset(list) {
    optExtensions.value = list.join(", ");
    browser.storage.local.set({ fileExtensions: list });
    showSaved();
  }

  // Site Rules
  function renderRulesTable() {
    if (siteRules.length === 0) {
      rulesTableBody.innerHTML = `
        <tr>
          <td colspan="3" style="text-align: center; color: var(--text-muted); padding: 18px;">
            No site rules configured yet.
          </td>
        </tr>
      `;
      return;
    }

    rulesTableBody.innerHTML = "";
    siteRules.forEach((rule, idx) => {
      const tr = document.createElement("tr");
      const isIntercept = rule.action === "intercept";
      tr.innerHTML = `
        <td style="font-family: monospace; font-weight: 500;">${escapeHtml(rule.pattern)}</td>
        <td>
          <span class="badge-action ${isIntercept ? "badge-intercept" : "badge-exclude"}">
            ${isIntercept ? "Intercept" : "Exclude"}
          </span>
        </td>
        <td>
          <button class="del-btn" data-idx="${idx}" title="Remove rule">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <polyline points="3 6 5 6 21 6"></polyline>
              <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path>
            </svg>
          </button>
        </td>
      `;

      tr.querySelector(".del-btn").addEventListener("click", (e) => {
        const btn = e.currentTarget;
        const i = parseInt(btn.dataset.idx, 10);
        siteRules.splice(i, 1);
        browser.storage.local.set({ siteRules });
        renderRulesTable();
        showSaved();
      });

      rulesTableBody.appendChild(tr);
    });
  }

  addRuleBtn.addEventListener("click", () => {
    const pattern = newRulePattern.value.trim().toLowerCase();
    if (!pattern) return;

    const action = newRuleAction.value;
    siteRules.push({ pattern, action, created_at: Date.now() });
    browser.storage.local.set({ siteRules });

    newRulePattern.value = "";
    renderRulesTable();
    showSaved();
  });

  newRulePattern.addEventListener("keydown", (e) => {
    if (e.key === "Enter") addRuleBtn.click();
  });

  // Diagnostics
  async function runDiagnostic() {
    diagStatus.textContent = "Connecting...";
    diagStatus.className = "diag-val";
    diagPort.textContent = "—";
    diagVersion.textContent = "—";

    try {
      const res = await browser.runtime.sendMessage({ type: "ping_host" });
      if (res && res.ok && res.data) {
        diagVersion.textContent = res.data.version || "v0.1.0";
        if (res.data.running) {
          diagStatus.textContent = "Connected & Engine Active";
          diagStatus.className = "diag-val connected";
          diagPort.textContent = String(res.data.port || "—");
        } else {
          diagStatus.textContent = "Host OK (Engine Idle / Offline)";
          diagStatus.className = "diag-val";
          diagPort.textContent = "None (Using Offline Database)";
        }
      } else {
        diagStatus.textContent = res?.error || "Host unreachable";
        diagStatus.className = "diag-val error";
      }
    } catch (e) {
      diagStatus.textContent = "Error testing connection";
      diagStatus.className = "diag-val error";
    }
  }

  testHostBtn.addEventListener("click", runDiagnostic);
  runDiagnostic();

  function escapeHtml(str) {
    return String(str)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }
});
