import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { AddInput, DesktopApi, Download, EngineEvent, EngineMode, LiveUpdate, SegmentRow } from "./types";

const SHELL_ERROR = {
  code: "shell",
  message: "Downloads run inside the desktop shell.",
};

function shellApi(): DesktopApi {
  return {
    mode: async () => "shell",
    list: async () => [],
    add: async () => {
      throw SHELL_ERROR;
    },
    pause: async () => {
      throw SHELL_ERROR;
    },
    resume: async () => {
      throw SHELL_ERROR;
    },
    remove: async () => {
      throw SHELL_ERROR;
    },
    segments: async () => [],
    settings: async () => ({}),
    setSetting: async () => {
      throw SHELL_ERROR;
    },
    defaultSaveDir: async () => "",
    subscribe: async () => () => {},
  };
}

function tauriApi(): DesktopApi {
  return {
    mode: () => invoke<EngineMode>("engine_mode"),
    list: () => invoke<Download[]>("list_downloads"),
    add: (input: AddInput) => invoke<{ id: string }>("add_download", { input }),
    pause: (id: string) => invoke("pause_download", { id }),
    resume: (id: string) => invoke("resume_download", { id }),
    remove: (id: string, deleteFile: boolean) => invoke("remove_download", { id, deleteFile }),
    segments: (id: string) => invoke<SegmentRow[]>("download_segments", { id }),
    settings: () => invoke<Record<string, unknown>>("get_settings"),
    setSetting: (key: string, value: unknown) => invoke("set_setting", { key, value }),
    defaultSaveDir: () => invoke<string>("default_save_dir"),
    subscribe: async (handler) => {
      const unlisten: UnlistenFn[] = [];
      unlisten.push(await listen<EngineEvent>("engine", (event) => handler({ kind: "event", event: event.payload })));
      unlisten.push(
        await listen<Download[]>("snapshot", (event) => handler({ kind: "snapshot", rows: event.payload })),
      );
      return () => {
        for (const stop of unlisten) stop();
      };
    },
  };
}

export function createApi(): DesktopApi {
  const marker = window as Window & { __TAURI_INTERNALS__?: unknown; __TAURI__?: unknown };
  if (marker.__TAURI_INTERNALS__ || marker.__TAURI__) return tauriApi();
  return shellApi();
}

export type { LiveUpdate };
