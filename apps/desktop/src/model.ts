import type { Download, DownloadStatus, EngineEvent, MapSegment, SegmentRow } from "./types";

export type ListFilter = "all" | "active" | "paused" | "completed" | "failed";

const FROZEN: DownloadStatus[] = ["paused", "completed", "failed", "stopped"];

export function isActive(status: DownloadStatus): boolean {
  return status === "queued" || status === "probing" || status === "ready" || status === "downloading";
}

export function matchesFilter(status: DownloadStatus, filter: ListFilter): boolean {
  if (filter === "all") return true;
  if (filter === "active") return isActive(status);
  if (filter === "paused") return status === "paused" || status === "stopped";
  if (filter === "completed") return status === "completed";
  return status === "failed";
}

/** Patch bytes from a progress event. A late event must not revive a paused or finished row. */
export function applyProgress(row: Download, event: EngineEvent): Download {
  if (event.type !== "progress" || event.id !== row.id) return row;
  if (FROZEN.includes(row.status)) return row;
  return {
    ...row,
    downloaded_bytes: event.downloaded_bytes ?? row.downloaded_bytes,
    total_bytes: event.total_bytes === undefined ? row.total_bytes : event.total_bytes,
    speed_bps: event.speed_bps,
    eta_ms: event.eta_ms,
  };
}

/** The engine already coalesces progress to about 8 Hz. Hide the window and drop to 1 Hz. */
export function shouldApplyProgress(hidden: boolean, lastAppliedAt: number, now: number): boolean {
  if (!hidden) return true;
  return now - lastAppliedAt >= 1000;
}

export interface VirtualRange {
  start: number;
  end: number;
  paddingTop: number;
  paddingBottom: number;
}

export function virtualRange(
  count: number,
  rowHeight: number,
  viewport: number,
  scrollTop: number,
  overscan: number,
): VirtualRange {
  if (count <= 0 || rowHeight <= 0) {
    return { start: 0, end: 0, paddingTop: 0, paddingBottom: 0 };
  }
  const scroll = Math.max(0, scrollTop);
  const first = Math.floor(scroll / rowHeight);
  const last = Math.ceil((scroll + Math.max(0, viewport)) / rowHeight);
  const start = Math.max(0, first - overscan);
  const end = Math.min(count, last + overscan);
  return {
    start,
    end,
    paddingTop: start * rowHeight,
    paddingBottom: Math.max(0, count - end) * rowHeight,
  };
}

export function pushSpeed(samples: readonly number[], speed: number, cap = 48): number[] {
  const next = samples.length >= cap ? samples.slice(samples.length - cap + 1) : samples.slice();
  next.push(speed);
  return next;
}

export function segmentFractions(
  segments: readonly MapSegment[],
  total: number | null,
): { offset: number; width: number; done: number }[] {
  const span = total ?? segments.reduce((max, segment) => Math.max(max, segment.end ?? segment.cursor), 0);
  if (span <= 0) return [];
  return segments.map((segment) => {
    const end = segment.end ?? span;
    const width = Math.max(0, end - segment.start) / span;
    const length = Math.max(1, end - segment.start);
    const done = Math.min(1, Math.max(0, (segment.cursor - segment.start) / length));
    return { offset: segment.start / span, width, done };
  });
}

export function toMapSegments(rows: readonly SegmentRow[]): MapSegment[] {
  return rows.map((row) => ({ start: row.start_byte, end: row.end_byte, cursor: row.cursor_byte }));
}

/** Keep the last speed sample across a list refetch, and drop it once the row is no longer active. */
export function mergeList(previous: readonly Download[], next: readonly Download[]): Download[] {
  const prior = new Map(previous.map((row) => [row.id, row]));
  return next.map((row) => {
    if (!isActive(row.status)) return { ...row, speed_bps: undefined, eta_ms: undefined };
    const old = prior.get(row.id);
    if (!old) return row;
    return { ...row, speed_bps: old.speed_bps, eta_ms: old.eta_ms };
  });
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const text = unit === 0 ? String(Math.round(value)) : value.toFixed(value >= 10 ? 1 : 2);
  return `${text} ${units[unit]}`;
}

export function formatEta(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return "—";
  const seconds = Math.round(ms / 1000);
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const rest = seconds % 60;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${rest}s`;
  return `${rest}s`;
}

export function readError(error: unknown): { code: string; message: string } {
  if (error && typeof error === "object" && "code" in error && "message" in error) {
    const record = error as { code: unknown; message: unknown };
    return { code: String(record.code), message: String(record.message) };
  }
  if (error instanceof Error) return { code: "failed", message: error.message };
  return { code: "failed", message: String(error) };
}
