import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { formatBytes, virtualRange } from "../model";
import type { Download } from "../types";

export const ROW_HEIGHT = 64;
export const OVERSCAN = 4;

export function DownloadList({
  rows,
  selectedId,
  onSelect,
  viewportHeight,
  scrollTop,
}: {
  rows: Download[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  viewportHeight?: number;
  scrollTop?: number;
}) {
  const { t } = useTranslation();
  const scroller = useRef<HTMLDivElement>(null);
  const [measured, setMeasured] = useState(viewportHeight ?? 400);
  const [scrolled, setScrolled] = useState(scrollTop ?? 0);

  useEffect(() => {
    if (viewportHeight !== undefined) setMeasured(viewportHeight);
  }, [viewportHeight]);

  useEffect(() => {
    if (scrollTop !== undefined) setScrolled(scrollTop);
  }, [scrollTop]);

  useEffect(() => {
    if (viewportHeight !== undefined) return;
    const node = scroller.current;
    if (!node || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => setMeasured(node.clientHeight));
    observer.observe(node);
    setMeasured(node.clientHeight);
    return () => observer.disconnect();
  }, [viewportHeight]);

  const range = virtualRange(rows.length, ROW_HEIGHT, measured, scrolled, OVERSCAN);

  return (
    <div
      ref={scroller}
      className="min-h-0 flex-1 overflow-auto"
      style={viewportHeight !== undefined ? { height: viewportHeight } : undefined}
      onScroll={(event) => {
        if (scrollTop === undefined) setScrolled(event.currentTarget.scrollTop);
      }}
    >
      <div style={{ height: range.paddingTop }} />
      {rows.slice(range.start, range.end).map((row, offset) => {
        const index = range.start + offset;
        const ratio = row.total_bytes ? Math.min(1, row.downloaded_bytes / Math.max(1, row.total_bytes)) : 0;
        const selected = row.id === selectedId;
        return (
          <button
            key={row.id}
            type="button"
            data-index={index}
            onClick={() => onSelect(row.id)}
            className={`flex w-full flex-col justify-center border-b border-line px-4 text-left ${selected ? "bg-paper-2" : "bg-paper"}`}
            style={{ height: ROW_HEIGHT }}
          >
            <span className="flex items-baseline justify-between gap-3">
              <span className="truncate font-medium">{row.filename}</span>
              <span className="shrink-0 text-sm text-stone">{t(`status.${row.status}`)}</span>
            </span>
            <span className="mt-1 h-1.5 overflow-hidden rounded-sm bg-line">
              <span className="block h-full bg-copper" style={{ width: `${ratio * 100}%` }} />
            </span>
            <span className="mt-1 text-xs text-stone">
              {formatBytes(row.downloaded_bytes)}
              {row.total_bytes != null ? ` / ${formatBytes(row.total_bytes)}` : ""}
            </span>
          </button>
        );
      })}
      <div style={{ height: range.paddingBottom }} />
    </div>
  );
}
