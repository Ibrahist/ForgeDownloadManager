export type DownloadStatus =
  | "queued"
  | "probing"
  | "ready"
  | "downloading"
  | "paused"
  | "completed"
  | "failed"
  | "stopped";

export type DuplicatePolicy = "rename" | "overwrite" | "ask";

export type EngineMode = "owner" | "forward" | "shell";

export interface Download {
  id: string;
  url: string;
  filename: string;
  save_dir: string;
  status: DownloadStatus;
  resumable: boolean;
  total_bytes: number | null;
  downloaded_bytes: number;
  error_code: string | null;
  error_message: string | null;
  speed_bps?: number;
  eta_ms?: number | null;
}

export interface SegmentRow {
  id: string;
  start_byte: number;
  end_byte: number | null;
  cursor_byte: number;
  status: string;
}

export interface MapSegment {
  start: number;
  end: number | null;
  cursor: number;
}

export interface EngineEvent {
  type: string;
  id: string;
  downloaded_bytes?: number;
  total_bytes?: number | null;
  speed_bps?: number;
  eta_ms?: number | null;
  segments?: MapSegment[];
  reason?: string;
  message?: string;
  filename?: string;
}

export interface AddInput {
  url: string;
  saveDir: string;
  filename?: string;
  segments?: number;
  referer?: string;
  userAgent?: string;
  headers?: string;
  cookies?: string;
  proxy?: string;
  speedLimitBps?: number;
  username?: string;
  password?: string;
  insecureTls: boolean;
  duplicatePolicy: DuplicatePolicy;
  checksumAlgo?: string;
  checksumExpected?: string;
}

export type LiveUpdate =
  | { kind: "event"; event: EngineEvent }
  | { kind: "snapshot"; rows: Download[] };

export interface DesktopApi {
  mode(): Promise<EngineMode>;
  list(): Promise<Download[]>;
  add(input: AddInput): Promise<{ id: string }>;
  pause(id: string): Promise<void>;
  resume(id: string): Promise<void>;
  remove(id: string, deleteFile: boolean): Promise<void>;
  segments(id: string): Promise<SegmentRow[]>;
  settings(): Promise<Record<string, unknown>>;
  setSetting(key: string, value: unknown): Promise<void>;
  defaultSaveDir(): Promise<string>;
  subscribe(handler: (update: LiveUpdate) => void): Promise<() => void>;
}
