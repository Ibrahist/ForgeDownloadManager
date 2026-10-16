import { useState } from "react";
import { useTranslation } from "react-i18next";

import { formatBytes, formatEta, isActive, segmentFractions } from "../model";
import type { Download, MapSegment } from "../types";

export function Details({
  download,
  segments,
  speeds,
  onPause,
  onResume,
  onRemove,
}: {
  download: Download | null;
  segments: MapSegment[];
  speeds: readonly number[];
  onPause: () => void;
  onResume: () => void;
  onRemove: (deleteFile: boolean) => void;
}) {
  const { t } = useTranslation();
  const [deleteFile, setDeleteFile] = useState(false);
  if (!download) {
    return <p className="p-4 text-sm text-stone">{t("details.empty")}</p>;
  }
  const parts = segmentFractions(segments, download.total_bytes);
  const maxSpeed = Math.max(1, ...speeds, 0);
  const width = 280;
  const height = 56;
  const points = speeds
    .map((sample, index) => {
      const x = speeds.length <= 1 ? 0 : (index / (speeds.length - 1)) * width;
      const y = height - (sample / maxSpeed) * (height - 2) - 1;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");

  return (
    <section className="flex h-full flex-col gap-4 overflow-auto p-4">
      <header>
        <h2 className="truncate text-lg font-semibold">{download.filename}</h2>
        <p className="truncate text-sm text-stone">{download.url}</p>
        <p className="mt-1 text-sm">
          {t(`status.${download.status}`)} · {formatBytes(download.downloaded_bytes)}
          {download.total_bytes != null ? ` / ${formatBytes(download.total_bytes)}` : ""}
          {download.speed_bps != null && isActive(download.status) ? ` · ${formatBytes(download.speed_bps)}/s` : ""}
        </p>
        <p className="text-sm text-stone">
          {t("details.eta")}: {formatEta(download.eta_ms)}
        </p>
      </header>
      {download.error_message ? (
        <p className="text-sm text-danger" role="alert">
          {t("details.error")}: {download.error_message}
        </p>
      ) : null}
      <div>
        <h3 className="mb-2 text-sm font-medium">{t("details.segmentMap")}</h3>
        {parts.length === 0 ? (
          <p className="text-sm text-stone">{t("details.noSegments")}</p>
        ) : (
          <div className="relative h-3 overflow-hidden rounded-sm bg-paper-2" role="img" aria-label={t("details.segmentMap")}>
            {parts.map((part, index) => (
              <span
                key={index}
                className="absolute inset-y-0 border-r border-paper"
                style={{ left: `${part.offset * 100}%`, width: `${Math.max(part.width * 100, 0.4)}%` }}
              >
                <span className="block h-full bg-copper" style={{ width: `${part.done * 100}%` }} />
              </span>
            ))}
          </div>
        )}
      </div>
      <div>
        <h3 className="mb-2 text-sm font-medium">{t("details.speed")}</h3>
        {speeds.length === 0 ? (
          <p className="text-sm text-stone">{t("details.noSpeed")}</p>
        ) : (
          <svg viewBox={`0 0 ${width} ${height}`} className="h-14 w-full" role="img" aria-label={t("details.speed")}>
            <polyline fill="none" stroke="currentColor" strokeWidth="2" className="text-copper" points={points} />
          </svg>
        )}
      </div>
      <div className="mt-auto flex flex-wrap items-center gap-2">
        {isActive(download.status) ? (
          <button type="button" className="rounded-md bg-ink px-3 py-1.5 text-paper" onClick={onPause}>
            {t("details.pause")}
          </button>
        ) : null}
        {download.status === "paused" || download.status === "failed" || download.status === "stopped" ? (
          <button type="button" className="rounded-md bg-copper px-3 py-1.5 text-paper" onClick={onResume}>
            {t("details.resume")}
          </button>
        ) : null}
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={deleteFile} onChange={(event) => setDeleteFile(event.target.checked)} />
          {t("details.deleteFile")}
        </label>
        <button type="button" className="rounded-md border border-danger px-3 py-1.5 text-danger" onClick={() => onRemove(deleteFile)}>
          {t("details.remove")}
        </button>
      </div>
    </section>
  );
}
