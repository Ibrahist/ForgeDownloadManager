import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { createApi } from "./bridge";
import { AddDialog } from "./components/AddDialog";
import { Details } from "./components/Details";
import { DownloadList } from "./components/DownloadList";
import { Mark } from "./components/Mark";
import { SettingsDrawer } from "./components/SettingsDrawer";
import { applyProgress, isActive, matchesFilter, mergeList, pushSpeed, readError, shouldApplyProgress, toMapSegments, type ListFilter } from "./model";
import type { DesktopApi, Download, EngineMode, LiveUpdate, MapSegment } from "./types";

const FILTERS: ListFilter[] = ["all", "active", "paused", "completed", "failed"];

export function App({ api }: { api?: DesktopApi }) {
  const { t } = useTranslation();
  const client = useMemo(() => api ?? createApi(), [api]);
  const [mode, setMode] = useState<EngineMode | null>(null);
  const [rows, setRows] = useState<Download[]>([]);
  const [filter, setFilter] = useState<ListFilter>("all");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [segments, setSegments] = useState<MapSegment[]>([]);
  const [speeds, setSpeeds] = useState<Record<string, number[]>>({});
  const [saveDir, setSaveDir] = useState("");
  const [addOpen, setAddOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [banner, setBanner] = useState("");
  const selectedRef = useRef<string | null>(null);
  const lastProgress = useRef(0);
  const lastSnapshot = useRef(0);
  selectedRef.current = selectedId;

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const pending: LiveUpdate[] = [];
    let accept = false;

    const apply = (update: LiveUpdate) => {
      if (update.kind === "snapshot") {
        const now = Date.now();
        if (!shouldApplyProgress(document.hidden, lastSnapshot.current, now)) return;
        lastSnapshot.current = now;
        setRows((current) => mergeList(current, update.rows));
        return;
      }
      const event = update.event;
      if (event.type !== "progress") {
        void client.list().then((next) => {
          if (!cancelled) setRows((current) => mergeList(current, next));
        }).catch(() => {});
        if (event.id === selectedRef.current) {
          void client.segments(event.id).then((next) => {
            if (!cancelled && event.id === selectedRef.current) setSegments(toMapSegments(next));
          }).catch(() => {});
        }
        return;
      }
      const now = Date.now();
      if (!shouldApplyProgress(document.hidden, lastProgress.current, now)) return;
      lastProgress.current = now;
      setRows((current) => current.map((row) => applyProgress(row, event)));
      if (typeof event.speed_bps === "number") {
        setSpeeds((current) => ({ ...current, [event.id]: pushSpeed(current[event.id] ?? [], event.speed_bps ?? 0) }));
      }
      if (event.segments && event.id === selectedRef.current) setSegments(event.segments);
    };

    void (async () => {
      try {
        const [nextMode, dir, listed, stop] = await Promise.all([
          client.mode(),
          client.defaultSaveDir(),
          client.list(),
          client.subscribe((update) => {
            if (!accept) pending.push(update);
            else apply(update);
          }),
        ]);
        if (cancelled) {
          stop();
          return;
        }
        unlisten = stop;
        setMode(nextMode);
        setSaveDir(dir);
        setRows(listed);
        if (nextMode === "shell") setBanner(t("app.shell"));
        else if (nextMode === "forward") setBanner(t("app.forward"));
        else setBanner("");
        accept = true;
        for (const update of pending) apply(update);
      } catch (error) {
        if (!cancelled) setBanner(`${t("app.bootError")} ${readError(error).message}`);
      }
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [client, t]);

  useEffect(() => {
    if (!selectedId || mode === "shell") {
      setSegments([]);
      return;
    }
    let cancelled = false;
    void client.segments(selectedId).then((next) => {
      if (!cancelled) setSegments(toMapSegments(next));
    }).catch(() => {
      if (!cancelled) setSegments([]);
    });
    return () => {
      cancelled = true;
    };
  }, [client, mode, selectedId]);

  const visible = rows.filter((row) => matchesFilter(row.status, filter));
  const selected = rows.find((row) => row.id === selectedId) ?? null;

  async function act(work: () => Promise<void>) {
    try {
      await work();
      setRows((current) => mergeList(current, current));
      const next = await client.list();
      setRows((current) => mergeList(current, next));
    } catch (error) {
      setBanner(readError(error).message);
    }
  }

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center gap-3 border-b border-line px-4 py-3">
        <Mark className="h-8 w-8 text-copper" />
        <h1 className="text-lg font-semibold tracking-tight">{t("app.name")}</h1>
        <div className="ml-auto flex gap-2">
          <button
            type="button"
            className="rounded-md bg-copper px-3 py-1.5 text-paper disabled:opacity-50"
            disabled={mode !== "owner" && mode !== "forward"}
            onClick={() => setAddOpen(true)}
          >
            {t("app.add")}
          </button>
          <button type="button" className="rounded-md border border-line px-3 py-1.5" onClick={() => setSettingsOpen(true)}>
            {t("app.settings")}
          </button>
        </div>
      </header>
      {banner ? <p className="border-b border-line bg-paper-2 px-4 py-2 text-sm">{banner}</p> : null}
      {mode === null && !banner ? <p className="px-4 py-2 text-sm text-stone">{t("app.loading")}</p> : null}
      <div className="flex gap-1 border-b border-line px-3 py-2">
        {FILTERS.map((item) => (
          <button
            key={item}
            type="button"
            aria-pressed={filter === item}
            className={`rounded-full px-3 py-1 text-sm ${filter === item ? "bg-ink text-paper" : "text-ink-soft"}`}
            onClick={() => setFilter(item)}
          >
            {t(`filter.${item}`)}
          </button>
        ))}
      </div>
      <div className="grid min-h-0 flex-1 grid-cols-1 md:grid-cols-[minmax(0,1.4fr)_minmax(18rem,0.8fr)]">
        {visible.length === 0 ? (
          <p className="p-4 text-sm text-stone">{t("app.empty")}</p>
        ) : (
          <DownloadList rows={visible} selectedId={selectedId} onSelect={setSelectedId} />
        )}
        <div className="min-h-0 border-t border-line md:border-t-0 md:border-l">
          <Details
            download={selected}
            segments={segments}
            speeds={selected ? (speeds[selected.id] ?? []) : []}
            onPause={() => {
              if (selected) void act(() => client.pause(selected.id));
            }}
            onResume={() => {
              if (selected && !isActive(selected.status)) void act(() => client.resume(selected.id));
            }}
            onRemove={(deleteFile) => {
              if (selected) void act(() => client.remove(selected.id, deleteFile));
            }}
          />
        </div>
      </div>
      {addOpen ? (
        <AddDialog
          api={client}
          open
          initialDir={saveDir}
          onClose={() => setAddOpen(false)}
          onAdded={() => {
            void client.list().then((next) => setRows((current) => mergeList(current, next))).catch(() => {});
          }}
        />
      ) : null}
      <SettingsDrawer api={client} open={settingsOpen} onClose={() => setSettingsOpen(false)} />
    </div>
  );
}
