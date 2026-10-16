import { describe, expect, it } from "vitest";

import { applyProgress, formatBytes, formatEta, matchesFilter, mergeList, pushSpeed, segmentFractions, shouldApplyProgress, virtualRange } from "./model";
import type { Download, EngineEvent } from "./types";

function download(overrides: Partial<Download> = {}): Download {
  return {
    id: "dl",
    url: "https://example.test/file",
    filename: "file.bin",
    save_dir: "C:\\Downloads",
    status: "downloading",
    resumable: true,
    total_bytes: 100,
    downloaded_bytes: 10,
    error_code: null,
    error_message: null,
    ...overrides,
  };
}

function progress(overrides: Partial<EngineEvent> = {}): EngineEvent {
  return {
    type: "progress",
    id: "dl",
    downloaded_bytes: 40,
    total_bytes: 100,
    speed_bps: 20,
    eta_ms: 3000,
    ...overrides,
  };
}

describe("applyProgress", () => {
  it("patches an active row and leaves its status alone", () => {
    const next = applyProgress(download(), progress());
    expect(next.downloaded_bytes).toBe(40);
    expect(next.speed_bps).toBe(20);
    expect(next.eta_ms).toBe(3000);
    expect(next.status).toBe("downloading");
  });

  it("ignores a late progress event for a paused row", () => {
    const paused = download({ status: "paused", downloaded_bytes: 12 });
    expect(applyProgress(paused, progress())).toEqual(paused);
  });

  it("ignores a different download", () => {
    const row = download();
    expect(applyProgress(row, progress({ id: "other" }))).toBe(row);
  });
});

describe("shouldApplyProgress", () => {
  it("always applies while the document is visible", () => {
    expect(shouldApplyProgress(false, 1000, 1100)).toBe(true);
  });

  it("drops hidden progress until a second has passed", () => {
    expect(shouldApplyProgress(true, 1000, 1999)).toBe(false);
    expect(shouldApplyProgress(true, 1000, 2000)).toBe(true);
  });
});

describe("virtualRange", () => {
  it("windows a long list with overscan", () => {
    const top = virtualRange(1000, 64, 400, 0, 4);
    expect(top.start).toBe(0);
    expect(top.end).toBe(11);
    expect(top.paddingTop).toBe(0);

    const scrolled = virtualRange(1000, 64, 400, 1000, 4);
    expect(scrolled.start).toBe(11);
    expect(scrolled.end).toBe(26);
    expect(scrolled.paddingTop).toBe(11 * 64);
    expect(scrolled.paddingBottom).toBe((1000 - 26) * 64);
  });
});

describe("pushSpeed", () => {
  it("caps the ring", () => {
    let samples: number[] = [];
    for (let i = 0; i < 60; i += 1) samples = pushSpeed(samples, i, 48);
    expect(samples).toHaveLength(48);
    expect(samples[0]).toBe(12);
    expect(samples[47]).toBe(59);
  });
});

describe("segmentFractions", () => {
  it("maps cursors onto the file", () => {
    const parts = segmentFractions(
      [
        { start: 0, end: 50, cursor: 50 },
        { start: 50, end: 100, cursor: 75 },
      ],
      100,
    );
    expect(parts[0]).toMatchObject({ offset: 0, width: 0.5, done: 1 });
    expect(parts[1]).toMatchObject({ offset: 0.5, width: 0.5, done: 0.5 });
  });
});

describe("formatters", () => {
  it("formats sizes and remaining time", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1536)).toBe("1.50 KB");
    expect(formatEta(null)).toBe("—");
    expect(formatEta(65_000)).toBe("1m 5s");
    expect(formatEta(3_661_000)).toBe("1h 1m");
  });
});

describe("mergeList", () => {
  it("keeps speed on active rows and clears it when they pause", () => {
    const previous = [download({ speed_bps: 80, eta_ms: 10 })];
    const active = mergeList(previous, [download({ downloaded_bytes: 20 })]);
    expect(active[0].speed_bps).toBe(80);
    const paused = mergeList(previous, [download({ status: "paused" })]);
    expect(paused[0].speed_bps).toBeUndefined();
  });
});

describe("matchesFilter", () => {
  it("treats queued work as active and stopped as paused", () => {
    expect(matchesFilter("queued", "active")).toBe(true);
    expect(matchesFilter("paused", "active")).toBe(false);
    expect(matchesFilter("stopped", "paused")).toBe(true);
    expect(matchesFilter("failed", "failed")).toBe(true);
  });
});
