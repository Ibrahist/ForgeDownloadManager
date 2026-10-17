// Forge Download Manager — Content Script
(() => {
  const browser = (typeof globalThis.browser !== "undefined" && globalThis.browser)
    ? globalThis.browser
    : globalThis.chrome;

  // Track Alt key press for download bypass
  window.addEventListener("keydown", (e) => {
    if (e.key === "Alt") {
      try {
        browser.runtime.sendMessage({ type: "set_alt_key", active: true });
      } catch {}
    }
  }, { passive: true });

  window.addEventListener("keyup", (e) => {
    if (e.key === "Alt") {
      try {
        browser.runtime.sendMessage({ type: "set_alt_key", active: false });
      } catch {}
    }
  }, { passive: true });

  // Listen for background requests (e.g. link extraction)
  browser.runtime.onMessage.addListener((msg, sender, sendResponse) => {
    if (msg.type === "extract_links") {
      const links = Array.from(document.querySelectorAll("a[href]"))
        .map(a => a.href)
        .filter(href => href && (href.startsWith("http://") || href.startsWith("https://")));
      const unique = Array.from(new Set(links));
      sendResponse({ links: unique });
      return true;
    }
  });

  // Media Sniffer: scan for video and audio elements
  const reportedMedia = new Set();

  function scanMedia() {
    const mediaElements = document.querySelectorAll("video, audio");
    mediaElements.forEach((el) => {
      let src = el.currentSrc || el.src;
      if (!src) {
        const source = el.querySelector("source");
        if (source) src = source.src;
      }

      if (src && (src.startsWith("http://") || src.startsWith("https://"))) {
        if (!reportedMedia.has(src)) {
          reportedMedia.add(src);

          const title = document.title || el.getAttribute("title") || "Media file";
          const isVideo = el.tagName.toLowerCase() === "video";
          const mediaInfo = {
            url: src,
            title,
            type: isVideo ? "video" : "audio",
            duration: el.duration && !isNaN(el.duration) ? Math.round(el.duration) : null
          };

          try {
            browser.runtime.sendMessage({ type: "media_detected", media: mediaInfo });
          } catch {}

          if (isVideo) {
            attachFloatingButton(el, src);
          }
        }
      }
    });
  }

  // Attach a floating Forge download badge to video element
  function attachFloatingButton(video, src) {
    if (video.dataset.forgeHooked) return;
    video.dataset.forgeHooked = "true";

    const badge = document.createElement("div");
    badge.className = "forge-media-badge";
    badge.innerHTML = `
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
        <polyline points="7 10 12 15 17 10"></polyline>
        <line x1="12" y1="15" x2="12" y2="3"></line>
      </svg>
      <span>Forge Download</span>
    `;

    badge.addEventListener("click", (e) => {
      e.stopPropagation();
      e.preventDefault();
      try {
        browser.runtime.sendMessage({
          type: "add_download",
          url: src,
          referer: window.location.href
        });
        badge.classList.add("forge-badge-sent");
        badge.querySelector("span").textContent = "Sent to Forge!";
        setTimeout(() => {
          badge.classList.remove("forge-badge-sent");
          badge.querySelector("span").textContent = "Forge Download";
        }, 2500);
      } catch {}
    });

    const parent = video.parentElement;
    if (parent) {
      if (window.getComputedStyle(parent).position === "static") {
        parent.style.position = "relative";
      }
      parent.appendChild(badge);
    }
  }

  // Initial scan and observer for dynamically inserted media
  scanMedia();
  const observer = new MutationObserver(() => scanMedia());
  observer.observe(document.body || document.documentElement, {
    childList: true,
    subtree: true
  });
})();
