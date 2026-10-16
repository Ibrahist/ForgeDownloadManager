import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { readError } from "../model";
import type { DesktopApi } from "../types";

export const SETTING_KEYS = [
  "proxy_mode",
  "proxy_url",
  "speed_limit_bps",
  "max_concurrent_downloads",
  "per_host_connections",
  "default_segments",
  "min_segment_bytes",
  "user_agent",
] as const;

function showValue(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  if (typeof value === "number") return String(value);
  return JSON.stringify(value);
}

function parseSetting(key: string, raw: string): unknown {
  const text = raw.trim();
  if (text === "" || text === "null") return null;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    if (key === "proxy_mode" || key === "proxy_url" || key === "user_agent") return text;
    throw new Error(`${key} value is not JSON`);
  }
}

export function SettingsDrawer({ api, open, onClose }: { api: DesktopApi; open: boolean; onClose: () => void }) {
  const { t } = useTranslation();
  const [values, setValues] = useState<Record<string, string>>({});
  const [error, setError] = useState("");
  const [saved, setSaved] = useState("");

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    void api.settings().then((loaded) => {
      if (cancelled) return;
      const next: Record<string, string> = {};
      for (const key of SETTING_KEYS) next[key] = showValue(loaded[key]);
      setValues(next);
    }).catch((caught: unknown) => {
      if (!cancelled) setError(readError(caught).message);
    });
    return () => {
      cancelled = true;
    };
  }, [api, open]);

  if (!open) return null;

  async function save(key: string) {
    setError("");
    setSaved("");
    try {
      await api.setSetting(key, parseSetting(key, values[key] ?? ""));
      setSaved(key);
    } catch (caught) {
      setError(readError(caught).message);
    }
  }

  return (
    <div className="fixed inset-0 z-20 flex justify-end bg-ink/40">
      <aside className="h-full w-full max-w-md overflow-auto bg-paper p-5 shadow-lg">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold">{t("settings.title")}</h2>
          <button type="button" onClick={onClose}>
            {t("settings.close")}
          </button>
        </div>
        <div className="mt-4 flex flex-col gap-3">
          {SETTING_KEYS.map((key) => (
            <label key={key} className="flex flex-col gap-1 text-sm">
              {t(`settings.${key}`)}
              <span className="flex gap-2">
                <input
                  aria-label={t(`settings.${key}`)}
                  className="min-w-0 flex-1 rounded-md border border-line bg-paper px-2 py-1.5"
                  value={values[key] ?? ""}
                  onChange={(event) => setValues((current) => ({ ...current, [key]: event.target.value }))}
                />
                <button type="button" className="rounded-md bg-ink px-3 py-1.5 text-paper" onClick={() => void save(key)}>
                  {saved === key ? t("settings.saved") : t("settings.save")}
                </button>
              </span>
            </label>
          ))}
        </div>
        {error ? (
          <p className="mt-3 text-sm text-danger" role="alert">
            {error}
          </p>
        ) : null}
      </aside>
    </div>
  );
}
