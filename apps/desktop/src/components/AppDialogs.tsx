import {
  AlertCircle,
  CheckCircle2,
  Download,
  Loader2,
  RefreshCw,
  RotateCcw,
  Settings,
  Sparkles,
} from "lucide-react";

import { INITIAL_APP_UPDATER_STATE, type AppUpdaterState } from "../appUpdater";
import type { Lang, StartupDiagnostics } from "../types";
import { Button, ModalShell } from "./ui";

export type AppToastProps = {
  lang: Lang;
  message: string;
  error: string;
  loading?: boolean;
  onDismissMessage: () => void;
  onDismissError: () => void;
};

export function AppToast({
  lang,
  message,
  error,
  loading = false,
  onDismissMessage,
  onDismissError,
}: AppToastProps) {
  const activeText = error || message;
  if (!activeText) return null;

  const isError = Boolean(error);
  const status = loading ? "loading" : isError ? "error" : "success";
  const [firstLine, ...remainingLines] = activeText.split("\n");
  const detail = remainingLines.join("\n").trim();
  const dismiss = isError ? onDismissError : onDismissMessage;

  return (
    <div
      key={`${status}:${activeText}`}
      className={`cx-app-toast cx-app-toast--${status}`}
      role={isError ? "alert" : "status"}
      aria-live={isError ? "assertive" : "polite"}
      onAnimationEnd={(event) => {
        if (event.target !== event.currentTarget || event.animationName !== "cx-app-toast-exit") return;
        dismiss();
      }}
    >
      {loading
        ? <Loader2 className="cx-app-toast-loader" size={18} aria-hidden="true" />
        : <span className="cx-app-toast-dot" aria-hidden="true" />}
      <div className="cx-app-toast-copy">
        <strong>{firstLine || (isError ? (lang === "zh" ? "操作失败" : "Action failed") : "Codex-X")}</strong>
        {detail && <span>{detail}</span>}
      </div>
    </div>
  );
}

export type UpdateDialogProps = {
  open: boolean;
  lang: Lang;
  state?: AppUpdaterState;
  currentVersion?: string | null;
  latestVersion?: string | null;
  onClose: () => void;
  onDownload: () => void;
  onUpdate?: () => void | Promise<unknown>;
  onRetry?: () => void | Promise<unknown>;
  onRestart?: () => void | Promise<unknown>;
};

function formatUpdateBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

export function UpdateDialog({
  open,
  lang,
  state,
  currentVersion,
  latestVersion,
  onClose,
  onDownload,
  onUpdate,
  onRetry,
  onRestart,
}: UpdateDialogProps) {
  const isChinese = lang === "zh";
  const updaterState = state ?? {
    ...INITIAL_APP_UPDATER_STATE,
    phase: "available" as const,
    currentVersion: currentVersion ?? null,
    latestVersion: latestVersion ?? null,
  };
  const phase = updaterState.phase;
  const isBusy = phase === "downloading" || phase === "installing";
  const totalBytes = updaterState.totalBytes;
  const hasKnownProgress = totalBytes !== null && totalBytes > 0;
  const progress = totalBytes !== null && totalBytes > 0
    ? Math.min(100, Math.round((updaterState.downloadedBytes / totalBytes) * 100))
    : null;

  const copy = isChinese
    ? {
        checkingTitle: "正在检查更新",
        checkingDescription: "正在确认是否有新版本。",
        availableTitle: "发现新版本",
        availableDescription: onUpdate
          ? "可以直接在软件内完成更新，无需重新下载安装包。"
          : "检测到新版本，可前往下载页获取对应平台的安装包。",
        downloadingTitle: "正在下载更新",
        downloadingDescription: "请保持 Codex-X 打开，下载完成后会自动安装。",
        installingTitle: "正在安装更新",
        installingDescription: "即将完成，请暂时不要关闭软件。",
        readyTitle: "更新已准备好",
        readyDescription: "重新启动 Codex-X 即可使用新版本。",
        errorTitle: "更新没有完成",
        errorDescription: updaterState.failure === "restart"
          ? "软件未能重新启动，请再试一次。"
          : "请重试；如果仍然失败，也可以前往下载页更新。",
        idleTitle: "当前已是最新版本",
        idleDescription: "暂时没有可用的新版本。",
        current: "当前版本",
        latest: "新版本",
        later: "稍后",
        close: "关闭",
        updateNow: "立即更新",
        downloading: "正在下载",
        installing: "正在安装",
        restart: "重新启动",
        retry: "重试",
        downloadPage: "打开下载页",
        releaseNotes: "本次更新",
      }
    : {
        checkingTitle: "Checking for updates",
        checkingDescription: "Checking whether a new version is available.",
        availableTitle: "New version available",
        availableDescription: onUpdate
          ? "Update directly in the app without downloading the installer again."
          : "A new version is available from the download page for your platform.",
        downloadingTitle: "Downloading update",
        downloadingDescription: "Keep Codex-X open. Installation starts automatically after download.",
        installingTitle: "Installing update",
        installingDescription: "Almost done. Please keep the app open.",
        readyTitle: "Update is ready",
        readyDescription: "Restart Codex-X to use the new version.",
        errorTitle: "Update did not finish",
        errorDescription: updaterState.failure === "restart"
          ? "Codex-X could not restart. Please try again."
          : "Try again, or use the download page if the problem continues.",
        idleTitle: "Codex-X is up to date",
        idleDescription: "There is no new version available right now.",
        current: "Current",
        latest: "New version",
        later: "Later",
        close: "Close",
        updateNow: "Update now",
        downloading: "Downloading",
        installing: "Installing",
        restart: "Restart",
        retry: "Try again",
        downloadPage: "Open download page",
        releaseNotes: "What's new",
      };

  const title = phase === "checking"
    ? copy.checkingTitle
    : phase === "available"
      ? copy.availableTitle
      : phase === "downloading"
        ? copy.downloadingTitle
        : phase === "installing"
          ? copy.installingTitle
          : phase === "ready"
            ? copy.readyTitle
            : phase === "error"
              ? copy.errorTitle
              : copy.idleTitle;
  const description = phase === "checking"
    ? copy.checkingDescription
    : phase === "available"
      ? copy.availableDescription
      : phase === "downloading"
        ? copy.downloadingDescription
        : phase === "installing"
          ? copy.installingDescription
          : phase === "ready"
            ? copy.readyDescription
            : phase === "error"
              ? copy.errorDescription
              : copy.idleDescription;

  const handleClose = () => {
    if (!isBusy) onClose();
  };

  const footer = phase === "available"
    ? (
        <>
          <Button variant="secondary" onClick={handleClose}>{copy.later}</Button>
          <Button
            icon={<Download size={16} />}
            onClick={() => {
              if (onUpdate) void onUpdate();
              else onDownload();
            }}
          >
            {onUpdate ? copy.updateNow : copy.downloadPage}
          </Button>
        </>
      )
    : phase === "ready"
      ? (
          <Button icon={<RefreshCw size={16} />} onClick={() => void onRestart?.()}>
            {copy.restart}
          </Button>
        )
      : phase === "error"
        ? (
            <>
              <Button variant="secondary" icon={<Download size={16} />} onClick={onDownload}>
                {copy.downloadPage}
              </Button>
              <Button icon={<RotateCcw size={16} />} onClick={() => void onRetry?.()}>
                {copy.retry}
              </Button>
            </>
          )
        : isBusy
          ? (
              <Button disabled icon={<Loader2 className="spin" size={16} />}>
                {phase === "downloading" ? copy.downloading : copy.installing}
              </Button>
            )
          : <Button variant="secondary" onClick={handleClose}>{copy.close}</Button>;

  return (
    <ModalShell
      open={open}
      onClose={handleClose}
      size="sm"
      title={title}
      description={description}
      closeLabel={copy.close}
      closeOnBackdrop={!isBusy}
      closeOnEscape={!isBusy}
      showCloseButton={!isBusy}
      className="cx-update-dialog"
      footer={footer}
    >
      <div className={`cx-update-dialog-icon cx-update-dialog-icon--${phase}`} aria-hidden="true">
        {phase === "checking" || isBusy
          ? <Loader2 className="spin" size={20} />
          : phase === "ready"
            ? <CheckCircle2 size={20} />
            : phase === "error"
              ? <AlertCircle size={20} />
              : <Sparkles size={20} />}
      </div>
      <dl className="cx-update-version-grid">
        <div><dt>{copy.current}</dt><dd>{updaterState.currentVersion || currentVersion || "-"}</dd></div>
        <div><dt>{copy.latest}</dt><dd>{updaterState.latestVersion || latestVersion || "-"}</dd></div>
      </dl>

      {(phase === "downloading" || phase === "installing" || phase === "ready") && (
        <div className="cx-update-progress" aria-live="polite">
          <div className="cx-update-progress-copy">
            <span>{phase === "downloading" ? copy.downloading : phase === "installing" ? copy.installing : copy.restart}</span>
            <strong>
              {phase === "downloading"
                ? hasKnownProgress
                  ? `${progress}% · ${formatUpdateBytes(updaterState.downloadedBytes)} / ${formatUpdateBytes(totalBytes)}`
                  : updaterState.downloadedBytes > 0
                    ? formatUpdateBytes(updaterState.downloadedBytes)
                    : "..."
                : phase === "ready"
                  ? "100%"
                  : "..."}
            </strong>
          </div>
          <div
            className={`cx-update-progress-track${progress === null && phase === "downloading" ? " cx-update-progress-track--indeterminate" : ""}`}
            role="progressbar"
            aria-label={phase === "downloading" ? copy.downloading : copy.installing}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={phase === "ready" ? 100 : progress ?? undefined}
          >
            <span style={{ width: phase === "ready" || phase === "installing" ? "100%" : progress === null ? "38%" : `${progress}%` }} />
          </div>
        </div>
      )}

      {updaterState.notes && phase !== "checking" && (
        <section className="cx-update-notes">
          <strong>{copy.releaseNotes}</strong>
          <p>{updaterState.notes}</p>
        </section>
      )}
    </ModalShell>
  );
}

export type StartupWizardDialogProps = {
  open: boolean;
  closing: boolean;
  lang: Lang;
  diagnostics: StartupDiagnostics | null;
  configDir: string;
  loading: boolean;
  onConfigDirChange: (value: string) => void;
  onRecheck: () => void;
  onSkip: () => void;
  onOpenSettings: () => void;
  onEnter: () => void;
};

export function StartupWizardDialog({
  open,
  closing,
  lang,
  diagnostics,
  configDir,
  loading,
  onConfigDirChange,
  onRecheck,
  onSkip,
  onOpenSettings,
  onEnter,
}: StartupWizardDialogProps) {
  const isChinese = lang === "zh";
  if (!diagnostics) return null;

  return (
    <ModalShell
      open={open}
      onClose={onSkip}
      size="lg"
      title={isChinese ? "首次启动向导" : "First-run wizard"}
      description={diagnostics.summary}
      showCloseButton={false}
      closeOnBackdrop={false}
      closeOnEscape={false}
      className={closing ? "cx-startup-dialog cx-startup-dialog--closing" : "cx-startup-dialog"}
      footer={(
        <>
          <Button variant="ghost" onClick={onSkip}>{isChinese ? "跳过" : "Skip"}</Button>
          <Button variant="secondary" icon={<Settings size={16} />} onClick={onOpenSettings}>{isChinese ? "去设置" : "Settings"}</Button>
          <Button icon={<CheckCircle2 size={16} />} onClick={onEnter}>{isChinese ? "进入 Codex-X" : "Enter Codex-X"}</Button>
        </>
      )}
    >
      <div className="cx-startup-path-control">
        <label htmlFor="cx-startup-codex-home">CODEX_HOME</label>
        <input
          id="cx-startup-codex-home"
          value={configDir || diagnostics.codexDir}
          onChange={(event) => onConfigDirChange(event.target.value)}
          placeholder="~/.codex"
          spellCheck={false}
        />
        <Button
          variant="secondary"
          icon={<RefreshCw size={16} className={loading ? "spin" : undefined} />}
          onClick={onRecheck}
          disabled={loading}
        >
          {isChinese ? "重新检测" : "Recheck"}
        </Button>
      </div>

      <div className="cx-startup-checks">
        {diagnostics.items.map((item) => {
          const isOk = item.status === "ok";
          const isManual = item.status === "manual";
          const statusText = isChinese
            ? item.message
            : isOk
              ? "Detected"
              : isManual
                ? "Manual selection required"
                : "Not found";
          return (
            <article className={`cx-startup-check${isOk ? " cx-startup-check--ok" : isManual ? " cx-startup-check--manual" : ""}`} key={item.key}>
              <div className="cx-startup-check-icon" aria-hidden="true">
                {isOk ? <CheckCircle2 size={17} /> : <AlertCircle size={17} />}
              </div>
              <div>
                <strong>{item.label}</strong>
                <p>{statusText}</p>
                {item.path && <code title={item.path}>{item.path}</code>}
              </div>
            </article>
          );
        })}
      </div>
    </ModalShell>
  );
}
