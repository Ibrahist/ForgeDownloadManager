import { useState } from "react";
import { useTranslation } from "react-i18next";

import { readError } from "../model";
import type { AddInput, DesktopApi, DuplicatePolicy } from "../types";

export function AddDialog({
  api,
  open,
  initialDir,
  onClose,
  onAdded,
}: {
  api: DesktopApi;
  open: boolean;
  initialDir: string;
  onClose: () => void;
  onAdded: (id: string) => void;
}) {
  const { t } = useTranslation();
  const [url, setUrl] = useState("");
  const [saveDir, setSaveDir] = useState(initialDir);
  const [filename, setFilename] = useState("");
  const [segments, setSegments] = useState("");
  const [cookies, setCookies] = useState("");
  const [headers, setHeaders] = useState("");
  const [speed, setSpeed] = useState("");
  const [proxy, setProxy] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [referer, setReferer] = useState("");
  const [userAgent, setUserAgent] = useState("");
  const [checksumAlgo, setChecksumAlgo] = useState("");
  const [checksumExpected, setChecksumExpected] = useState("");
  const [policy, setPolicy] = useState<DuplicatePolicy>("rename");
  const [insecure, setInsecure] = useState(false);
  const [confirmInsecure, setConfirmInsecure] = useState(false);
  const [ask, setAsk] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  if (!open) return null;

  function input(policyOverride: DuplicatePolicy): AddInput {
    const segmentCount = segments.trim() === "" ? undefined : Number(segments);
    const speedLimit = speed.trim() === "" ? undefined : Number(speed);
    return {
      url: url.trim(),
      saveDir: saveDir.trim(),
      filename: filename.trim() || undefined,
      segments: segmentCount !== undefined && Number.isFinite(segmentCount) ? segmentCount : undefined,
      cookies: cookies.trim() || undefined,
      headers: headers.trim() || undefined,
      speedLimitBps: speedLimit !== undefined && Number.isFinite(speedLimit) ? speedLimit : undefined,
      proxy: proxy.trim() || undefined,
      username: username.trim() || undefined,
      password: password || undefined,
      referer: referer.trim() || undefined,
      userAgent: userAgent.trim() || undefined,
      checksumAlgo: checksumAlgo || undefined,
      checksumExpected: checksumExpected.trim() || undefined,
      insecureTls: insecure,
      duplicatePolicy: policyOverride,
    };
  }

  async function submit(policyOverride: DuplicatePolicy) {
    setBusy(true);
    setError("");
    try {
      const created = await api.add(input(policyOverride));
      onAdded(created.id);
      onClose();
    } catch (caught) {
      const parsed = readError(caught);
      if (parsed.code === "duplicate_ask") {
        setAsk(true);
      } else {
        setError(parsed.message);
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="fixed inset-0 z-20 flex items-start justify-center overflow-auto bg-ink/40 p-6">
      <form
        className="w-full max-w-2xl rounded-lg bg-paper p-5 shadow-lg"
        autoComplete="off"
        onSubmit={(event) => {
          event.preventDefault();
          void submit(policy);
        }}
      >
        <h2 className="text-lg font-semibold">{t("add.title")}</h2>
        <div className="mt-4 grid gap-3 sm:grid-cols-2">
          <label className="sm:col-span-2 flex flex-col gap-1 text-sm">
            {t("add.url")}
            <input required className="rounded-md border border-line bg-paper px-2 py-1.5" value={url} onChange={(event) => setUrl(event.target.value)} />
          </label>
          <label className="sm:col-span-2 flex flex-col gap-1 text-sm">
            {t("add.saveDir")}
            <input required className="rounded-md border border-line bg-paper px-2 py-1.5" value={saveDir} onChange={(event) => setSaveDir(event.target.value)} />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            {t("add.filename")}
            <input className="rounded-md border border-line bg-paper px-2 py-1.5" value={filename} onChange={(event) => setFilename(event.target.value)} />
            <span className="text-xs text-stone">{t("add.filenameHint")}</span>
          </label>
          <label className="flex flex-col gap-1 text-sm">
            {t("add.segments")}
            <input min={1} max={32} type="number" className="rounded-md border border-line bg-paper px-2 py-1.5" value={segments} onChange={(event) => setSegments(event.target.value)} />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            {t("add.cookies")}
            <input className="rounded-md border border-line bg-paper px-2 py-1.5" value={cookies} onChange={(event) => setCookies(event.target.value)} placeholder={t("add.cookiesHint")} />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            {t("add.speed")}
            <input type="number" min={0} className="rounded-md border border-line bg-paper px-2 py-1.5" value={speed} onChange={(event) => setSpeed(event.target.value)} />
          </label>
          <label className="sm:col-span-2 flex flex-col gap-1 text-sm">
            {t("add.headers")}
            <textarea className="min-h-16 rounded-md border border-line bg-paper px-2 py-1.5" value={headers} onChange={(event) => setHeaders(event.target.value)} placeholder={t("add.headersHint")} />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            {t("add.proxy")}
            <input className="rounded-md border border-line bg-paper px-2 py-1.5" value={proxy} onChange={(event) => setProxy(event.target.value)} placeholder={t("add.proxyHint")} />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            {t("add.referer")}
            <input className="rounded-md border border-line bg-paper px-2 py-1.5" value={referer} onChange={(event) => setReferer(event.target.value)} />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            {t("add.username")}
            <input className="rounded-md border border-line bg-paper px-2 py-1.5" value={username} onChange={(event) => setUsername(event.target.value)} />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            {t("add.password")}
            <input type="password" className="rounded-md border border-line bg-paper px-2 py-1.5" value={password} onChange={(event) => setPassword(event.target.value)} />
            <span className="text-xs text-stone">{t("add.passwordHint")}</span>
          </label>
          <label className="flex flex-col gap-1 text-sm">
            {t("add.userAgent")}
            <input className="rounded-md border border-line bg-paper px-2 py-1.5" value={userAgent} onChange={(event) => setUserAgent(event.target.value)} />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            {t("add.checksum")}
            <select className="rounded-md border border-line bg-paper px-2 py-1.5" value={checksumAlgo} onChange={(event) => setChecksumAlgo(event.target.value)}>
              <option value="">{t("add.checksumNone")}</option>
              <option value="md5">MD5</option>
              <option value="sha1">SHA-1</option>
              <option value="sha256">SHA-256</option>
            </select>
          </label>
          <label className="sm:col-span-2 flex flex-col gap-1 text-sm">
            {t("add.checksumValue")}
            <input className="rounded-md border border-line bg-paper px-2 py-1.5" value={checksumExpected} onChange={(event) => setChecksumExpected(event.target.value)} />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            {t("add.duplicatePolicy")}
            <select aria-label={t("add.duplicatePolicy")} className="rounded-md border border-line bg-paper px-2 py-1.5" value={policy} onChange={(event) => setPolicy(event.target.value as DuplicatePolicy)}>
              <option value="rename">{t("add.policyRename")}</option>
              <option value="overwrite">{t("add.policyOverwrite")}</option>
              <option value="ask">{t("add.policyAsk")}</option>
            </select>
          </label>
          <label className="flex items-center gap-2 self-end text-sm">
            <input
              type="checkbox"
              checked={insecure}
              onChange={(event) => {
                if (event.target.checked) setConfirmInsecure(true);
                else setInsecure(false);
              }}
            />
            {t("add.insecure")}
          </label>
        </div>
        {error ? (
          <p className="mt-3 text-sm text-danger" role="alert">
            {error}
          </p>
        ) : null}
        <div className="mt-4 flex justify-end gap-2">
          <button type="button" className="rounded-md px-3 py-1.5" onClick={onClose}>
            {t("add.cancel")}
          </button>
          <button type="submit" disabled={busy} className="rounded-md bg-copper px-3 py-1.5 text-paper disabled:opacity-60">
            {busy ? t("add.working") : t("add.submit")}
          </button>
        </div>
      </form>
      {confirmInsecure ? (
        <div role="dialog" aria-labelledby="insecure-title" className="fixed inset-0 z-30 flex items-center justify-center bg-ink/50 p-6">
          <div className="max-w-md rounded-lg bg-paper p-5">
            <h2 id="insecure-title" className="text-lg font-semibold">
              {t("add.insecureTitle")}
            </h2>
            <p className="mt-2 text-sm">{t("add.insecureBody")}</p>
            <div className="mt-4 flex justify-end gap-2">
              <button type="button" className="rounded-md px-3 py-1.5" onClick={() => setConfirmInsecure(false)}>
                {t("add.insecureCancel")}
              </button>
              <button
                type="button"
                className="rounded-md bg-danger px-3 py-1.5 text-paper"
                onClick={() => {
                  setInsecure(true);
                  setConfirmInsecure(false);
                }}
              >
                {t("add.insecureContinue")}
              </button>
            </div>
          </div>
        </div>
      ) : null}
      {ask ? (
        <div role="dialog" aria-labelledby="duplicate-title" className="fixed inset-0 z-30 flex items-center justify-center bg-ink/50 p-6">
          <div className="max-w-md rounded-lg bg-paper p-5">
            <h2 id="duplicate-title" className="text-lg font-semibold">
              {t("add.duplicateTitle")}
            </h2>
            <p className="mt-2 text-sm">{t("add.duplicateBody")}</p>
            <div className="mt-4 flex justify-end gap-2">
              <button type="button" onClick={() => setAsk(false)}>
                {t("add.duplicateCancel")}
              </button>
              <button type="button" className="rounded-md border border-line px-3 py-1.5" onClick={() => void submit("rename")}>
                {t("add.useRename")}
              </button>
              <button type="button" className="rounded-md bg-copper px-3 py-1.5 text-paper" onClick={() => void submit("overwrite")}>
                {t("add.useOverwrite")}
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
