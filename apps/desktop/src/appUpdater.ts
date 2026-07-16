import { useSyncExternalStore } from "react";
import {
  check as checkForTauriUpdate,
  type CheckOptions,
  type DownloadEvent,
  type DownloadOptions,
  type Update,
} from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

export type AppUpdaterPhase =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "installing"
  | "ready"
  | "error";

export type AppUpdaterFailure = "check" | "download" | "install" | "restart" | null;

export type AppUpdaterState = Readonly<{
  phase: AppUpdaterPhase;
  currentVersion: string | null;
  latestVersion: string | null;
  notes: string | null;
  publishedAt: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
  failure: AppUpdaterFailure;
}>;

export type AppUpdaterCheckResult = "available" | "up-to-date" | "error";

export type AppUpdaterCheckOptions = CheckOptions & {
  force?: boolean;
};

export const INITIAL_APP_UPDATER_STATE: AppUpdaterState = {
  phase: "idle",
  currentVersion: null,
  latestVersion: null,
  notes: null,
  publishedAt: null,
  downloadedBytes: 0,
  totalBytes: null,
  failure: null,
};

type Listener = () => void;
type RetryAction = "check" | "update" | "restart";

function normalizeByteCount(value: number | undefined): number | null {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : null;
}

class AppUpdaterController {
  private state: AppUpdaterState = INITIAL_APP_UPDATER_STATE;
  private readonly listeners = new Set<Listener>();
  private update: Update | null = null;
  private checkPromise: Promise<AppUpdaterCheckResult> | null = null;
  private updatePromise: Promise<AppUpdaterPhase> | null = null;
  private restartPromise: Promise<AppUpdaterPhase> | null = null;
  private retryAction: RetryAction = "check";

  readonly getSnapshot = (): AppUpdaterState => this.state;

  readonly subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  readonly check = (options: AppUpdaterCheckOptions = {}): Promise<AppUpdaterCheckResult> => {
    if (this.checkPromise) return this.checkPromise;

    if (this.isUpdateBusy()) {
      return Promise.resolve(this.update ? "available" : "error");
    }

    if (this.update && !options.force) return Promise.resolve("available");

    const { force: _force, ...pluginOptions } = options;
    this.checkPromise = this.performCheck(pluginOptions).finally(() => {
      this.checkPromise = null;
    });
    return this.checkPromise;
  };

  readonly downloadAndInstall = (options?: DownloadOptions): Promise<AppUpdaterPhase> => {
    if (this.updatePromise) return this.updatePromise;
    if (this.state.phase === "ready") return Promise.resolve("ready");

    if (!this.update) {
      this.retryAction = "check";
      this.setState({ phase: "error", failure: "check" });
      return Promise.resolve("error");
    }

    this.updatePromise = this.performDownloadAndInstall(this.update, options).finally(() => {
      this.updatePromise = null;
    });
    return this.updatePromise;
  };

  readonly retry = (): Promise<AppUpdaterCheckResult | AppUpdaterPhase> => {
    if (this.retryAction === "restart") return this.restart();
    if (this.retryAction === "update" && this.update) return this.downloadAndInstall();
    return this.check({ force: true });
  };

  readonly restart = (): Promise<AppUpdaterPhase> => {
    if (this.restartPromise) return this.restartPromise;
    if (this.state.phase !== "ready" && this.state.failure !== "restart") {
      return Promise.resolve(this.state.phase);
    }

    this.restartPromise = this.performRestart().finally(() => {
      this.restartPromise = null;
    });
    return this.restartPromise;
  };

  private isUpdateBusy(): boolean {
    return this.state.phase === "downloading" || this.state.phase === "installing";
  }

  private async performCheck(options: CheckOptions): Promise<AppUpdaterCheckResult> {
    this.retryAction = "check";
    this.setState({
      phase: "checking",
      downloadedBytes: 0,
      totalBytes: null,
      failure: null,
    });

    try {
      const nextUpdate = await checkForTauriUpdate(options);
      if (!nextUpdate) {
        await this.replaceUpdate(null);
        this.setState({
          phase: "idle",
          latestVersion: null,
          notes: null,
          publishedAt: null,
          failure: null,
        });
        return "up-to-date";
      }

      await this.replaceUpdate(nextUpdate);
      this.retryAction = "update";
      this.setState({
        phase: "available",
        currentVersion: nextUpdate.currentVersion,
        latestVersion: nextUpdate.version,
        notes: nextUpdate.body?.trim() || null,
        publishedAt: nextUpdate.date || null,
        downloadedBytes: 0,
        totalBytes: null,
        failure: null,
      });
      return "available";
    } catch {
      await this.replaceUpdate(null);
      this.setState({ phase: "error", failure: "check" });
      return "error";
    }
  }

  private async performDownloadAndInstall(update: Update, options?: DownloadOptions): Promise<AppUpdaterPhase> {
    this.retryAction = "update";
    let downloadFinished = false;
    this.setState({
      phase: "downloading",
      downloadedBytes: 0,
      totalBytes: null,
      failure: null,
    });

    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Finished") downloadFinished = true;
        this.handleDownloadEvent(event);
      }, options);
      this.setState({
        phase: "ready",
        downloadedBytes: this.state.totalBytes ?? this.state.downloadedBytes,
        failure: null,
      });
      return "ready";
    } catch {
      const failedDuringInstall = downloadFinished || this.state.phase === "installing";
      this.setState({
        phase: "error",
        failure: failedDuringInstall ? "install" : "download",
      });
      return "error";
    }
  }

  private handleDownloadEvent(event: DownloadEvent): void {
    if (event.event === "Started") {
      this.setState({
        phase: "downloading",
        downloadedBytes: 0,
        totalBytes: normalizeByteCount(event.data.contentLength),
      });
      return;
    }

    if (event.event === "Progress") {
      const nextBytes = this.state.downloadedBytes + Math.max(0, event.data.chunkLength);
      this.setState({
        downloadedBytes: this.state.totalBytes === null
          ? nextBytes
          : Math.min(nextBytes, this.state.totalBytes),
      });
      return;
    }

    this.setState({
      phase: "installing",
      downloadedBytes: this.state.totalBytes ?? this.state.downloadedBytes,
    });
  }

  private async performRestart(): Promise<AppUpdaterPhase> {
    this.retryAction = "restart";
    this.setState({ failure: null });
    try {
      await relaunch();
      return "ready";
    } catch {
      this.setState({ phase: "error", failure: "restart" });
      return "error";
    }
  }

  private async replaceUpdate(nextUpdate: Update | null): Promise<void> {
    const previousUpdate = this.update;
    this.update = nextUpdate;
    if (!previousUpdate || previousUpdate === nextUpdate) return;

    try {
      await previousUpdate.close();
    } catch (error) {
      console.warn("Unable to release the previous updater resource", error);
    }
  }

  private setState(patch: Partial<AppUpdaterState>): void {
    this.state = { ...this.state, ...patch };
    this.listeners.forEach((listener) => listener());
  }
}

export const appUpdater = new AppUpdaterController();

export function useAppUpdater() {
  const state = useSyncExternalStore(appUpdater.subscribe, appUpdater.getSnapshot, appUpdater.getSnapshot);
  return {
    state,
    check: appUpdater.check,
    downloadAndInstall: appUpdater.downloadAndInstall,
    retry: appUpdater.retry,
    restart: appUpdater.restart,
  } as const;
}
