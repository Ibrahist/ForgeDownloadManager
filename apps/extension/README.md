# Forge Download Manager — Browser Extension

Manifest V3 browser integration for Forge Download Manager. Supports Google Chrome, Microsoft Edge, Brave, and Mozilla Firefox.

## Features

- **Automatic Download Interception:** Intercepts downloadable files (archives, media, executables, documents) and forwards them directly to Forge for multi-segment acceleration.
- **Alt Key Bypass:** Hold `Alt` while clicking any link to bypass Forge and let the browser download natively.
- **Cookie & Referer Forwarding:** Automatically extracts domain session cookies and referer headers, ensuring private and authenticated downloads succeed.
- **Media Sniffer:** Detects HTML5 `<video>`, `<audio>`, HLS, and DASH streams on the active page, providing a floating quick-download badge and popup grabber list.
- **Context Menus:** Right-click any link or media item to download directly with Forge.
- **Site Rules:** Custom domain rules to force interception or exclude sites.
- **Native Messaging Host:** High-performance binary stdio communication with the Forge engine. Falls back to offline SQLite queuing if the desktop app is not running.

## Installation

### 1. Register Native Messaging Host

Run the Forge CLI host installer to register the native messaging host manifest for your browser:

```bash
forge-dm host --install
```

### 2. Load the Extension in Your Browser

#### Google Chrome / Microsoft Edge / Brave:
1. Open `chrome://extensions` or `edge://extensions`.
2. Enable **Developer mode** (top right switch).
3. Click **Load unpacked**.
4. Select the `apps/extension` directory in this repository.

#### Mozilla Firefox:
1. Open `about:debugging#/runtime/this-firefox`.
2. Click **Load Temporary Add-on...**.
3. Select `apps/extension/manifest.json`.

## Structure

```
apps/extension/
├── manifest.json          # Manifest V3 specification
├── icons/                 # Extension icons (16, 32, 48, 128px)
├── background/
│   ├── background.js      # Service worker, native messaging, interception
│   └── shim.js            # Direct chrome/browser compatibility shim
├── content/
│   ├── content.js         # DOM media sniffer, link extractor, Alt-key tracker
│   └── content.css        # Floating video download button styles
├── popup/
│   ├── popup.html         # Quick overview, media grabber, page links tabs
│   ├── popup.css          # Premium dark theme styling
│   └── popup.js           # Popup controller and state management
└── options/
    ├── options.html       # Interception settings, file types, site rules
    ├── options.css        # Full options page stylesheet
    └── options.js         # Settings sync & diagnostic connection test
```
