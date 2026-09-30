import {
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  Download,
  Folder,
  FolderTree,
  History,
  Info,
  Loader2,
  Pin,
  RefreshCw,
  Search,
  Trash2,
  Zap,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button, Checkbox, ModalShell, cx } from "../components/ui";
import "../styles/session-management.css";

const SESSION_ROW_HEIGHT = 58;
const SESSION_COLUMN_HEADER_HEIGHT = 34;
const SESSION_VIRTUAL_OVERSCAN_ROWS = 8;
const DEFAULT_SESSION_VIEWPORT_HEIGHT = 640;
const SESSION_PINS_STORAGE_PREFIX = "codexx.sessionPins.v1";
const SESSION_FOLDER_UNKNOWN_KEY = "__codexx_no_workspace__";
const SESSION_ALL_FOLDERS_KEY = "__codexx_all_folders__";

type SessionPinPreferences = {
  folders: string[];
  sessions: string[];
};

type ScopedSessionPinPreferences = SessionPinPreferences & {
  storageKey: string;
};

function normalizeFolderPinKey(value?: string | null) {
  const normalized = (value || "").trim().replace(/\\/g, "/").replace(/\/+$/, "");
  return normalized ? normalized.toLocaleLowerCase() : SESSION_FOLDER_UNKNOWN_KEY;
}

function sessionPinStorageKey(codexDir: string) {
  return `${SESSION_PINS_STORAGE_PREFIX}:${encodeURIComponent(normalizeFolderPinKey(codexDir))}`;
}

function readSessionPinPreferences(storageKey: string): SessionPinPreferences {
  try {
    const parsed = JSON.parse(localStorage.getItem(storageKey) || "{}") as Partial<SessionPinPreferences>;
    return {
      folders: Array.isArray(parsed.folders) ? parsed.folders.filter((value): value is string => typeof value === "string") : [],
      sessions: Array.isArray(parsed.sessions) ? parsed.sessions.filter((value): value is string => typeof value === "string") : [],
    };
  } catch {
    return { folders: [], sessions: [] };
  }
}

function folderDisplayName(value: string, missing: string) {
  if (!value) return missing;
  const parts = value.replace(/\\/g, "/").split("/").filter(Boolean);
  return parts[parts.length - 1] || missing;
}

type SessionVirtualRow = {
  kind: "session";
  key: string;
  top: number;
  height: number;
  item: SessionPreview;
};

function findVirtualRowIndex(rows: SessionVirtualRow[], offset: number) {
  let low = 0;
  let high = rows.length;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    if (rows[middle].top + rows[middle].height <= offset) low = middle + 1;
    else high = middle;
  }
  return low;
}

export type Lang = "zh" | "en";

export type SessionPreview = {
  id: string;
  title: string;
  modelProvider?: string | null;
  model?: string | null;
  cwd?: string | null;
  rolloutPath?: string | null;
  updatedAtMs?: number | null;
  archived: boolean;
  hasUserEvent: boolean;
  isSubagent: boolean;
  needsSync: boolean;
};

export type SessionSyncStatus = {
  codexDir: string;
  targetProvider: string;
  rolloutFiles: number;
  sessionMetaCount: number;
  mismatchedRollouts: number;
  mismatchedSessionMeta: number;
  sqliteDbs: number;
  sqliteThreads: number;
  topLevelThreads: number;
  subagentThreads: number;
  mismatchedThreads: number;
  mismatchedSessions: number;
  needsSync: boolean;
  scanComplete: boolean;
  scanFailures: string[];
  backupDir?: string | null;
  warnings: string[];
  sessions: SessionPreview[];
};

type SessionManagementPageProps = {
  active: boolean;
  lang: Lang;
  sessionStatus: SessionSyncStatus | null;
  sessionHasMismatches: boolean;
  sessionSyncCount: number;
  sessionTargetLabel: string;
  sessionVisibleTotal: number;
  sessionPreviewTruncated: boolean;
  visibleSessions: SessionPreview[];
  filteredSessions: SessionPreview[];
  allSessionsByCwd: Map<string, SessionPreview[]>;
  groupedSessions: Array<[string, SessionPreview[]]>;
  selectedSessionIds: string[];
  selectedSessionSet: Set<string>;
  selectedSessions: SessionPreview[];
  sessionQuery: string;
  sessionGroupByCwd: boolean;
  showInternalSessions: boolean;
  loading: boolean;
  actionBusy: string;
  sessionDeleteConfirmOpen: boolean;
  sessionDeleteBusy: boolean;
  sessionExportBusy: boolean;
  sessionDeleteSafetyConfirmed: boolean;
  onCheckSessions: () => void;
  onSyncSessions: () => void;
  onSessionQueryChange: (value: string) => void;
  onSessionGroupByCwdChange: (checked: boolean) => void;
  onShowInternalSessionsChange: (checked: boolean) => void;
  onOpenDeleteConfirm: () => void;
  onToggleSessionSelected: (id: string) => void;
  onSetSessionGroupSelected: (sessions: SessionPreview[], checked: boolean) => void;
  onCloseDeleteConfirm: () => void;
  onDeleteSelectedSessions: () => void;
  onExportSessions: (ids: string[]) => void;
  onDeleteSafetyConfirmedChange: (checked: boolean) => void;
};

function formatSessionTime(value?: number | null, lang: Lang = "zh") {
  if (!value) return lang === "zh" ? "未知时间" : "Unknown time";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return lang === "zh" ? "未知时间" : "Unknown time";
  return date.toLocaleString(lang === "zh" ? "zh-CN" : undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function compactPath(value: string | null | undefined, max = 58, missing = "未记录路径") {
  if (!value) return missing;
  const normalized = value.replace(/\\/g, "/");
  if (normalized.length <= max) return normalized;
  const parts = normalized.split("/").filter(Boolean);
  if (parts.length >= 3) {
    const tail = parts.slice(-3).join("/");
    return `…/${tail}`;
  }
  return `…${normalized.slice(-max + 1)}`;
}

function shortId(value: string) {
  return value.length > 13 ? `${value.slice(0, 8)}…${value.slice(-4)}` : value;
}

export function SessionManagementPage({
  active,
  lang,
  sessionStatus,
  sessionHasMismatches,
  sessionSyncCount,
  sessionTargetLabel,
  sessionVisibleTotal,
  sessionPreviewTruncated,
  visibleSessions,
  filteredSessions,
  allSessionsByCwd,
  groupedSessions,
  selectedSessionIds,
  selectedSessionSet,
  selectedSessions,
  sessionQuery,
  sessionGroupByCwd,
  showInternalSessions,
  loading,
  actionBusy,
  sessionDeleteConfirmOpen,
  sessionDeleteBusy,
  sessionExportBusy,
  sessionDeleteSafetyConfirmed,
  onCheckSessions,
  onSyncSessions,
  onSessionQueryChange,
  onSessionGroupByCwdChange,
  onShowInternalSessionsChange,
  onOpenDeleteConfirm,
  onToggleSessionSelected,
  onSetSessionGroupSelected,
  onCloseDeleteConfirm,
  onDeleteSelectedSessions,
  onExportSessions,
  onDeleteSafetyConfirmedChange,
}: SessionManagementPageProps) {
  const isChinese = lang === "zh";
  const codexDir = sessionStatus?.codexDir || "default";
  const copy = isChinese
    ? {
        syncEyebrow: "会话同步",
        title: "会话管理",
        description: "同步普通会话到共享列表，内部任务不参与同步。不会修改聊天内容。",
        syncTo: "同步到",
        check: "检查会话",
        checking: "检查中...",
        sync: "同步会话",
        syncing: "同步中...",
        clickToCheck: "点击检查会话",
        scanIncomplete: "无法确认同步状态，请查看下方原因",
        needsSync: (count: number) => `有 ${count} 条会话需要同步`,
        allSynced: "全部会话已同步",
        sessionCount: (count: number) => `${count} 条会话`,
        local: "本地会话",
        list: "文件夹与会话",
        shown: (shown: number, total: number) => `展示 ${shown} / ${total} 条`,
        loaded: (count: number) => `当前加载 ${count} 条`,
        search: "搜索标题 / 项目 / 供应商 / ID",
        groupByProject: "文件夹视图",
        showInternal: (count: number) => `显示内部会话 (${count})`,
        deleteSelected: "删除选中",
        exportSelected: "导出选中",
        exportOne: "导出为 Markdown",
        exporting: "导出中…",
        exportHint: "单个会话保存为 Markdown；多个会话打包为 ZIP",
        deleteMany: (count: number) => `永久删除 ${count} 条`,
        selectAll: "选择当前列表中的全部会话",
        selectProject: (path: string, count: number) => `选择项目 ${path} 的 ${count} 条会话`,
        projectCount: (count: number, truncated: boolean) => `${truncated ? "已加载 " : ""}${count} 条`,
        projectShown: (shown: number, total: number) => `显示 ${shown} / 共 ${total} 条`,
        pinFolder: "置顶文件夹",
        unpinFolder: "取消置顶文件夹",
        pinSession: "置顶会话",
        unpinSession: "取消置顶会话",
        allSessions: "全部会话",
        folders: "工作文件夹",
        selectSession: "选择会话",
        archived: "已归档",
        internal: "内部",
        internalHint: "内部任务仅供查看，不参与会话同步。",
        pending: "待同步",
        unknownProvider: "未知供应商",
        noModel: "未记录",
        noMatch: "没有匹配的会话。",
        noSessions: "还没有读取到会话。点击右上角“检查会话”刷新。",
        loadingMore: "正在加载更多会话…",
        diagnostics: "诊断信息",
        diagnosticsCount: (count: number) => `${count} 条 · 点击查看`,
        deleteTitle: (count: number) => `永久删除 ${count} 条会话`,
        irreversible: "此操作不可恢复",
        deleteDescription: "所选会话将从 Codex 的本地数据中永久删除，不会移入回收站，也不会创建新的备份。",
        deleteChildren: "由这些会话派生的子会话也会一并删除。",
        closeClients: "请先关闭正在使用这些会话的 Codex 窗口或 CLI。",
        pendingDelete: "待删除会话",
        moreSessions: (count: number) => `另有 ${count} 条会话未在此处展开`,
        safetyCheck: "我已关闭其他正在使用这些会话的 Codex 窗口或 CLI",
        cancel: "取消",
        deleting: "正在永久删除...",
        confirmDelete: (count: number) => `确认永久删除 ${count} 条`,
      }
    : {
        syncEyebrow: "SESSION SYNC",
        title: "Session management",
        description: "Keep user conversations in one shared history. Internal tasks are excluded from sync; chat content is unchanged.",
        syncTo: "Sync to",
        check: "Check sessions",
        checking: "Checking...",
        sync: "Sync sessions",
        syncing: "Syncing...",
        clickToCheck: "Check sessions to get started",
        scanIncomplete: "Unable to verify sync status. See the reason below.",
        needsSync: (count: number) => `${count} session(s) need syncing`,
        allSynced: "All sessions are synced",
        sessionCount: (count: number) => `${count} sessions`,
        local: "LOCAL SESSIONS",
        list: "Folders & sessions",
        shown: (shown: number, total: number) => `${shown} / ${total} shown`,
        loaded: (count: number) => `${count} loaded`,
        search: "Search title / project / provider / ID",
        groupByProject: "Folder view",
        showInternal: (count: number) => `Show internal sessions (${count})`,
        deleteSelected: "Delete selected",
        exportSelected: "Export selected",
        exportOne: "Export as Markdown",
        exporting: "Exporting…",
        exportHint: "Save one session as Markdown, or multiple sessions in a ZIP",
        deleteMany: (count: number) => `Delete ${count} permanently`,
        selectAll: "Select all sessions in the current list",
        selectProject: (path: string, count: number) => `Select ${count} sessions in ${path}`,
        projectCount: (count: number, truncated: boolean) => `${count}${truncated ? " loaded" : ""}`,
        projectShown: (shown: number, total: number) => `${shown} / ${total} shown`,
        pinFolder: "Pin folder",
        unpinFolder: "Unpin folder",
        pinSession: "Pin session",
        unpinSession: "Unpin session",
        allSessions: "All sessions",
        folders: "Workspace folders",
        selectSession: "Select session",
        archived: "Archived",
        internal: "Internal",
        internalHint: "Internal tasks are available for inspection and excluded from session sync.",
        pending: "Needs sync",
        unknownProvider: "Unknown provider",
        noModel: "Not recorded",
        noMatch: "No matching sessions.",
        noSessions: "No sessions loaded. Click Check sessions to refresh.",
        loadingMore: "Loading more sessions…",
        diagnostics: "Diagnostics",
        diagnosticsCount: (count: number) => `${count} · click to view`,
        deleteTitle: (count: number) => `Permanently delete ${count} session(s)`,
        irreversible: "This cannot be undone",
        deleteDescription: "Selected sessions will be permanently deleted from Codex local data. There is no recycle bin or new backup.",
        deleteChildren: "Child sessions spawned from these sessions will also be deleted.",
        closeClients: "Close other Codex windows or CLIs using these sessions first.",
        pendingDelete: "Sessions to delete",
        moreSessions: (count: number) => `${count} more session(s) not shown`,
        safetyCheck: "I closed other Codex windows or CLIs using these sessions",
        cancel: "Cancel",
        deleting: "Deleting permanently...",
        confirmDelete: (count: number) => `Delete ${count} permanently`,
      };

  const scanIncomplete = Boolean(sessionStatus && !sessionStatus.scanComplete);
  const diagnostics = [
    ...(sessionStatus?.scanFailures || []).map((message) => ({ message, blocking: true })),
    ...(sessionStatus?.warnings || []).map((message) => ({ message, blocking: false })),
  ];
  const dialogOpen = sessionDeleteConfirmOpen && selectedSessions.length > 0;
  const sessionScrollRef = useRef<HTMLDivElement | null>(null);
  const pinStorageKey = useMemo(() => sessionPinStorageKey(codexDir), [codexDir]);
  const [pinPreferences, setPinPreferences] = useState<ScopedSessionPinPreferences>({
    storageKey: "",
    folders: [],
    sessions: [],
  });
  const [selectedFolderKey, setSelectedFolderKey] = useState(SESSION_ALL_FOLDERS_KEY);
  const [sessionScrollMetrics, setSessionScrollMetrics] = useState({
    scrollTop: 0,
    viewportHeight: DEFAULT_SESSION_VIEWPORT_HEIGHT,
  });

  useEffect(() => {
    setPinPreferences({ storageKey: pinStorageKey, ...readSessionPinPreferences(pinStorageKey) });
    setSelectedFolderKey(SESSION_ALL_FOLDERS_KEY);
  }, [pinStorageKey]);

  const activePinPreferences = pinPreferences.storageKey === pinStorageKey
    ? pinPreferences
    : { storageKey: pinStorageKey, folders: [], sessions: [] };
  const pinnedFolderSet = useMemo(() => new Set(activePinPreferences.folders), [activePinPreferences.folders]);
  const pinnedSessionSet = useMemo(() => new Set(activePinPreferences.sessions), [activePinPreferences.sessions]);

  const updatePins = useCallback((kind: "folders" | "sessions", key: string) => {
    setPinPreferences((current) => {
      const base = current.storageKey === pinStorageKey
        ? current
        : { storageKey: pinStorageKey, ...readSessionPinPreferences(pinStorageKey) };
      const values = new Set(base[kind]);
      if (values.has(key)) values.delete(key);
      else values.add(key);
      const next = { ...base, [kind]: Array.from(values) };
      try {
        localStorage.setItem(pinStorageKey, JSON.stringify({ folders: next.folders, sessions: next.sessions }));
      } catch {
        // Pinning remains available for this run even when localStorage is unavailable.
      }
      return next;
    });
  }, [pinStorageKey]);

  const orderedSessionGroups = useMemo(() => groupedSessions.map(([group, items]) => {
    const folderKey = normalizeFolderPinKey(items.find((item) => item.cwd)?.cwd);
    const orderedItems = [...items].sort((left, right) => {
      const pinnedOrder = Number(pinnedSessionSet.has(right.id)) - Number(pinnedSessionSet.has(left.id));
      if (pinnedOrder) return pinnedOrder;
      const recencyOrder = (right.updatedAtMs || 0) - (left.updatedAtMs || 0);
      return recencyOrder || left.id.localeCompare(right.id);
    });
    return {
      group,
      folderKey,
      items: orderedItems,
      newestAt: orderedItems.reduce((latest, item) => Math.max(latest, item.updatedAtMs || 0), 0),
      pinned: pinnedFolderSet.has(folderKey),
    };
  }).sort((left, right) => {
    if (!sessionGroupByCwd) return 0;
    const pinnedOrder = Number(right.pinned) - Number(left.pinned);
    return pinnedOrder || right.newestAt - left.newestAt || left.group.localeCompare(right.group);
  }), [groupedSessions, pinnedFolderSet, pinnedSessionSet, sessionGroupByCwd]);

  const orderedAllSessions = useMemo(() => [...filteredSessions].sort((left, right) => {
    const pinnedOrder = Number(pinnedSessionSet.has(right.id)) - Number(pinnedSessionSet.has(left.id));
    if (pinnedOrder) return pinnedOrder;
    const recencyOrder = (right.updatedAtMs || 0) - (left.updatedAtMs || 0);
    return recencyOrder || left.id.localeCompare(right.id);
  }), [filteredSessions, pinnedSessionSet]);

  useEffect(() => {
    if (!sessionGroupByCwd || selectedFolderKey === SESSION_ALL_FOLDERS_KEY) return;
    if (!orderedSessionGroups.some((group) => group.folderKey === selectedFolderKey)) {
      setSelectedFolderKey(SESSION_ALL_FOLDERS_KEY);
    }
  }, [orderedSessionGroups, selectedFolderKey, sessionGroupByCwd]);

  const displayedSessions = useMemo(() => {
    if (!sessionGroupByCwd || selectedFolderKey === SESSION_ALL_FOLDERS_KEY) return orderedAllSessions;
    return orderedSessionGroups.find((group) => group.folderKey === selectedFolderKey)?.items || [];
  }, [orderedAllSessions, orderedSessionGroups, selectedFolderKey, sessionGroupByCwd]);
  const selectedVisibleCount = displayedSessions.filter((item) => selectedSessionSet.has(item.id)).length;
  const allVisibleSelected = displayedSessions.length > 0 && selectedVisibleCount === displayedSessions.length;
  const visibleSelectionIsPartial = selectedVisibleCount > 0 && !allVisibleSelected;

  const virtualSessionModel = useMemo(() => {
    const rows: SessionVirtualRow[] = [];
    let top = 0;
    displayedSessions.forEach((item) => {
      rows.push({
        kind: "session",
        key: `session:${item.id}`,
        top,
        height: SESSION_ROW_HEIGHT,
        item,
      });
      top += SESSION_ROW_HEIGHT;
    });
    return { rows, totalHeight: top };
  }, [displayedSessions]);

  const visibleSessionRows = useMemo(() => {
    const viewportHeight = sessionScrollMetrics.viewportHeight || DEFAULT_SESSION_VIEWPORT_HEIGHT;
    const bodyScrollTop = Math.max(0, sessionScrollMetrics.scrollTop - SESSION_COLUMN_HEADER_HEIGHT);
    const firstVisibleIndex = findVirtualRowIndex(virtualSessionModel.rows, bodyScrollTop);
    const lastVisibleIndex = findVirtualRowIndex(virtualSessionModel.rows, bodyScrollTop + viewportHeight);
    const firstIndex = Math.max(0, firstVisibleIndex - SESSION_VIRTUAL_OVERSCAN_ROWS);
    const lastIndex = Math.min(
      virtualSessionModel.rows.length,
      lastVisibleIndex + SESSION_VIRTUAL_OVERSCAN_ROWS + 1,
    );
    return virtualSessionModel.rows.slice(firstIndex, lastIndex);
  }, [sessionScrollMetrics, virtualSessionModel]);

  const syncSessionScrollMetrics = useCallback(() => {
    const element = sessionScrollRef.current;
    if (!element) return;
    setSessionScrollMetrics((current) => {
      const next = {
        scrollTop: element.scrollTop,
        viewportHeight: element.clientHeight,
      };
      return current.scrollTop === next.scrollTop && current.viewportHeight === next.viewportHeight
        ? current
        : next;
    });
  }, []);

  const handleSessionScroll = useCallback(() => {
    syncSessionScrollMetrics();
  }, [syncSessionScrollMetrics]);

  useEffect(() => {
    syncSessionScrollMetrics();
    const element = sessionScrollRef.current;
    if (!element) return undefined;
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", syncSessionScrollMetrics);
      return () => window.removeEventListener("resize", syncSessionScrollMetrics);
    }
    const observer = new ResizeObserver(syncSessionScrollMetrics);
    observer.observe(element);
    return () => observer.disconnect();
  }, [active, syncSessionScrollMetrics]);

  useEffect(() => {
    const element = sessionScrollRef.current;
    if (!element) return;
    const maxScrollTop = Math.max(0, element.scrollHeight - element.clientHeight);
    if (element.scrollTop > maxScrollTop) {
      element.scrollTop = maxScrollTop;
      syncSessionScrollMetrics();
    }
  }, [syncSessionScrollMetrics, virtualSessionModel.totalHeight]);

  useEffect(() => {
    const element = sessionScrollRef.current;
    if (!element) return;
    element.scrollTop = 0;
    syncSessionScrollMetrics();
  }, [selectedFolderKey, sessionGroupByCwd, syncSessionScrollMetrics]);

  return (
    <>
      <ModalShell
        open={dialogOpen}
        onClose={onCloseDeleteConfirm}
        title={copy.deleteTitle(selectedSessions.length)}
        description={copy.deleteDescription}
        size="lg"
        closeLabel={isChinese ? "关闭" : "Close"}
        closeOnBackdrop={!sessionDeleteBusy}
        closeOnEscape={!sessionDeleteBusy}
        showCloseButton={!sessionDeleteBusy}
        className="cx-session-delete-dialog"
        bodyClassName="cx-session-delete-modal-body"
        footer={(
          <>
            <Button variant="secondary" onClick={onCloseDeleteConfirm} disabled={sessionDeleteBusy} data-initial-focus>
              {copy.cancel}
            </Button>
            <Button
              variant="danger"
              className="cx-session-delete-confirm"
              icon={sessionDeleteBusy ? <Loader2 size={16} className="cx-session-spin" aria-hidden="true" /> : <Trash2 size={16} aria-hidden="true" />}
              onClick={onDeleteSelectedSessions}
              disabled={sessionDeleteBusy || !sessionDeleteSafetyConfirmed}
            >
              {sessionDeleteBusy ? copy.deleting : copy.confirmDelete(selectedSessions.length)}
            </Button>
          </>
        )}
      >
        <div className="cx-session-delete-warning">
          <AlertCircle size={19} strokeWidth={1.9} aria-hidden="true" />
          <div>
            <strong>{copy.irreversible}</strong>
            <p>{copy.deleteChildren}</p>
            <p>{copy.closeClients}</p>
          </div>
        </div>

        <div className="cx-session-delete-list" aria-label={copy.pendingDelete}>
          {selectedSessions.slice(0, 8).map((item) => (
            <div className="cx-session-delete-item" key={item.id}>
              <strong title={item.title}>{item.title || (isChinese ? "未命名会话" : "Untitled session")}</strong>
              <code>#{shortId(item.id)}</code>
              <span title={item.cwd || item.rolloutPath || undefined}>
                {compactPath(item.cwd || item.rolloutPath, 72, isChinese ? "未记录路径" : "No path recorded")}
              </span>
            </div>
          ))}
          {selectedSessions.length > 8 && <p className="cx-session-delete-more">{copy.moreSessions(selectedSessions.length - 8)}</p>}
        </div>

        <Checkbox
          className="cx-session-safety-check"
          checked={sessionDeleteSafetyConfirmed}
          onCheckedChange={onDeleteSafetyConfirmedChange}
          disabled={sessionDeleteBusy}
          label={copy.safetyCheck}
        />
      </ModalShell>

      <section className={cx("cx-session-page", !active && "page-pane-hidden")}>
        <header className="cx-session-header">
          <div className="cx-session-heading">
            <p className="cx-session-eyebrow"><RefreshCw size={13} strokeWidth={2} aria-hidden="true" />{copy.syncEyebrow}</p>
            <h2>{copy.title}</h2>
            <p className="cx-session-description">{copy.description}</p>
          </div>
          <div className="cx-session-header-actions">
            <span className="cx-session-target"><span>{copy.syncTo}</span><strong>{sessionTargetLabel}</strong></span>
            <button type="button" className="cx-session-button cx-session-button--secondary" onClick={onCheckSessions} disabled={loading} aria-busy={actionBusy === "checkSessions"}>
              {actionBusy === "checkSessions" ? <Loader2 size={16} className="cx-session-spin" aria-hidden="true" /> : <RefreshCw size={16} aria-hidden="true" />}
              {actionBusy === "checkSessions" ? copy.checking : copy.check}
            </button>
            <button type="button" className="cx-session-button cx-session-button--primary" onClick={onSyncSessions} disabled={loading || scanIncomplete || !sessionHasMismatches} aria-busy={actionBusy === "syncSessions"}>
              {actionBusy === "syncSessions" ? <Loader2 size={16} className="cx-session-spin" aria-hidden="true" /> : <Zap size={16} aria-hidden="true" />}
              {actionBusy === "syncSessions" ? copy.syncing : copy.sync}
            </button>
          </div>
        </header>

        <div className={cx("cx-session-summary", scanIncomplete || sessionHasMismatches ? "cx-session-summary--needs-sync" : "cx-session-summary--synced")}>
          <span className="cx-session-summary-status">
            {!sessionStatus ? <Info size={15} aria-hidden="true" /> : scanIncomplete || sessionHasMismatches ? <AlertCircle size={15} aria-hidden="true" /> : <CheckCircle2 size={15} aria-hidden="true" />}
            {!sessionStatus ? copy.clickToCheck : scanIncomplete ? copy.scanIncomplete : sessionHasMismatches ? copy.needsSync(sessionSyncCount) : copy.allSynced}
          </span>
          <span className="cx-session-summary-count">{copy.sessionCount(sessionStatus?.topLevelThreads ?? 0)}</span>
        </div>

        <div className="cx-session-list-card">
          <div className="cx-session-list-heading">
            <div>
              <p className="cx-session-section-label">{copy.local}</p>
              <h3>{copy.list}</h3>
            </div>
            <span
              className="cx-session-total"
              title={sessionPreviewTruncated ? copy.loaded(visibleSessions.length) : undefined}
            >
              {copy.shown(filteredSessions.length, sessionVisibleTotal)}
            </span>
          </div>

          <div className="cx-session-toolbar">
            <label className="cx-session-search">
              <Search size={16} strokeWidth={1.9} aria-hidden="true" />
              <input
                value={sessionQuery}
                onChange={(event) => onSessionQueryChange(event.target.value)}
                placeholder={copy.search}
                aria-label={copy.search}
              />
            </label>
            <Checkbox
              className={cx("cx-session-toggle", sessionGroupByCwd && "cx-session-toggle--active")}
              checked={sessionGroupByCwd}
              onCheckedChange={onSessionGroupByCwdChange}
              label={<><FolderTree size={15} strokeWidth={1.9} aria-hidden="true" /><span>{copy.groupByProject}</span></>}
            />
            {(sessionStatus?.subagentThreads ?? 0) > 0 && (
              <Checkbox
                className={cx("cx-session-toggle", showInternalSessions && "cx-session-toggle--active")}
                checked={showInternalSessions}
                onCheckedChange={onShowInternalSessionsChange}
                title={copy.internalHint}
                label={copy.showInternal(sessionStatus?.subagentThreads ?? 0)}
              />
            )}
            <button
              type="button"
              className="cx-session-button cx-session-button--secondary"
              onClick={() => onExportSessions(selectedSessionIds)}
              disabled={loading || sessionDeleteBusy || sessionExportBusy || selectedSessionIds.length === 0}
              title={copy.exportHint}
              aria-busy={sessionExportBusy}
            >
              {sessionExportBusy ? <Loader2 size={15} className="cx-session-spin" aria-hidden="true" /> : <Download size={15} strokeWidth={1.9} aria-hidden="true" />}
              {sessionExportBusy ? copy.exporting : copy.exportSelected}
            </button>
            <button
              type="button"
              className={cx("cx-session-button cx-session-delete-trigger", selectedSessionIds.length > 0 ? "cx-session-button--danger" : "cx-session-button--secondary")}
              onClick={onOpenDeleteConfirm}
              disabled={loading || sessionDeleteBusy || sessionExportBusy || selectedSessionIds.length === 0}
              title={selectedSessionIds.length > 0 ? undefined : copy.deleteSelected}
            >
              <Trash2 size={15} strokeWidth={1.9} aria-hidden="true" />
              {selectedSessionIds.length > 0 ? copy.deleteMany(selectedSessionIds.length) : copy.deleteSelected}
            </button>
          </div>

          <div className={cx("cx-session-browser", !sessionGroupByCwd && "cx-session-browser--flat")}>
            {sessionGroupByCwd && (
              <aside className="cx-session-folder-tree" aria-label={copy.folders}>
                <div className="cx-session-tree-heading">
                  <ChevronDown size={14} strokeWidth={2} aria-hidden="true" />
                  <FolderTree size={15} strokeWidth={1.9} aria-hidden="true" />
                  <span>{copy.folders}</span>
                </div>
                <div className="cx-session-tree-items" role="tree">
                  <button
                    type="button"
                    role="treeitem"
                    aria-selected={selectedFolderKey === SESSION_ALL_FOLDERS_KEY}
                    className={cx("cx-session-tree-all", selectedFolderKey === SESSION_ALL_FOLDERS_KEY && "cx-session-tree-item--active")}
                    onClick={() => setSelectedFolderKey(SESSION_ALL_FOLDERS_KEY)}
                  >
                    <History size={15} strokeWidth={1.9} aria-hidden="true" />
                    <span>{copy.allSessions}</span>
                    <em>{filteredSessions.length}</em>
                  </button>
                  {orderedSessionGroups.map((folder) => (
                    <div
                      className={cx("cx-session-tree-item", selectedFolderKey === folder.folderKey && "cx-session-tree-item--active")}
                      role="treeitem"
                      aria-selected={selectedFolderKey === folder.folderKey}
                      key={folder.folderKey}
                    >
                      <button
                        type="button"
                        className="cx-session-tree-select"
                        onClick={() => setSelectedFolderKey(folder.folderKey)}
                        title={folder.group}
                      >
                        <Folder size={15} strokeWidth={1.8} aria-hidden="true" />
                        <span className="cx-session-folder-copy">
                          <strong>{folderDisplayName(folder.group, isChinese ? "未记录路径" : "No path recorded")}</strong>
                          <small>{compactPath(folder.group, 34, isChinese ? "未记录路径" : "No path recorded")}</small>
                        </span>
                        <em>{folder.items.length}</em>
                      </button>
                      <button
                        type="button"
                        className={cx("cx-session-pin-action", folder.pinned && "cx-session-pin-action--active")}
                        onClick={() => updatePins("folders", folder.folderKey)}
                        aria-label={folder.pinned ? copy.unpinFolder : copy.pinFolder}
                        title={folder.pinned ? copy.unpinFolder : copy.pinFolder}
                      >
                        <Pin size={13} strokeWidth={1.9} fill={folder.pinned ? "currentColor" : "none"} aria-hidden="true" />
                      </button>
                    </div>
                  ))}
                </div>
              </aside>
            )}

            <div className="cx-session-results">
              {displayedSessions.length > 0 ? (
                <div
                  ref={sessionScrollRef}
                  className="cx-session-scroll"
                  role="table"
                  aria-label={copy.list}
                  onScroll={handleSessionScroll}
                >
                  <div className="cx-session-column-head" role="row">
                    <Checkbox
                      className="cx-session-select-all"
                      checked={allVisibleSelected}
                      indeterminate={visibleSelectionIsPartial}
                      onCheckedChange={(checked) => onSetSessionGroupSelected(displayedSessions, checked)}
                      aria-label={copy.selectAll}
                      disabled={loading || sessionDeleteBusy}
                    />
                    <span>{isChinese ? "会话" : "Session"}</span>
                    <span>{isChinese ? "更新时间" : "Updated"}</span>
                    <span>{isChinese ? "供应商" : "Provider"}</span>
                    <span>{isChinese ? "模型" : "Model"}</span>
                    <span className="cx-session-id-heading">ID</span>
                    <span className="cx-session-actions-heading">{isChinese ? "操作" : "Actions"}</span>
                  </div>
                  <div className="cx-session-table-body" style={{ height: virtualSessionModel.totalHeight }}>
                    {visibleSessionRows.map((row) => {
                      const { item } = row;
                      const sessionPinned = pinnedSessionSet.has(item.id);
                      return (
                        <div
                          className={cx(
                            "cx-session-row",
                            "cx-session-virtual-item",
                            item.needsSync && !item.isSubagent && "cx-session-row--needs-sync",
                            selectedSessionSet.has(item.id) && "cx-session-row--selected",
                            sessionPinned && "cx-session-row--pinned",
                          )}
                          key={row.key}
                          role="row"
                          style={{ top: row.top, height: row.height }}
                          onClick={(event) => {
                            if (!(event.target as HTMLElement).closest("button, input") && !loading && !sessionDeleteBusy) onToggleSessionSelected(item.id);
                          }}
                        >
                          <span className="cx-session-select-box" title={copy.selectSession}>
                            <input
                              className="cx-session-checkbox"
                              type="checkbox"
                              checked={selectedSessionSet.has(item.id)}
                              disabled={loading || sessionDeleteBusy}
                              onChange={() => onToggleSessionSelected(item.id)}
                              aria-label={`${copy.selectSession}: ${item.title || (isChinese ? "未命名会话" : "Untitled session")} (#${shortId(item.id)})`}
                            />
                          </span>
                          <div className="cx-session-row-copy">
                            <div className="cx-session-row-title">
                              <strong title={item.title}>{item.title || (isChinese ? "未命名会话" : "Untitled session")}</strong>
                              {item.archived && <span className="cx-session-state">{copy.archived}</span>}
                              {item.isSubagent && <span className="cx-session-state" title={copy.internalHint}>{copy.internal}</span>}
                              {item.needsSync && !item.isSubagent && <span className="cx-session-state cx-session-state--warn">{copy.pending}</span>}
                            </div>
                            {(!sessionGroupByCwd || selectedFolderKey === SESSION_ALL_FOLDERS_KEY) && <p title={item.cwd || item.rolloutPath || undefined}>{compactPath(item.cwd || item.rolloutPath, 72, isChinese ? "未记录路径" : "No path recorded")}</p>}
                          </div>
                          <span className="cx-session-meta cx-session-meta--time" title={item.updatedAtMs ? new Date(item.updatedAtMs).toLocaleString() : undefined}>{formatSessionTime(item.updatedAtMs, lang)}</span>
                          <code className="cx-session-meta cx-session-meta--provider" title={item.modelProvider || undefined}>{item.modelProvider || copy.unknownProvider}</code>
                          <span className="cx-session-meta cx-session-meta--model" title={item.model || undefined}>{item.model || copy.noModel}</span>
                          <small className="cx-session-meta cx-session-meta--id" title={item.id}>#{shortId(item.id)}</small>
                          <div className="cx-session-row-actions">
                            <button
                              type="button"
                              className={cx("cx-session-pin-action", sessionPinned && "cx-session-pin-action--active")}
                              onClick={() => updatePins("sessions", item.id)}
                              aria-label={sessionPinned ? copy.unpinSession : copy.pinSession}
                              title={sessionPinned ? copy.unpinSession : copy.pinSession}
                            >
                              <Pin size={14} strokeWidth={1.9} fill={sessionPinned ? "currentColor" : "none"} aria-hidden="true" />
                            </button>
                            <button
                              type="button"
                              className="cx-session-row-export"
                              onClick={() => onExportSessions([item.id])}
                              disabled={loading || sessionDeleteBusy || sessionExportBusy}
                              aria-label={`${copy.exportOne}: ${item.title || (isChinese ? "未命名会话" : "Untitled session")}`}
                              title={copy.exportOne}
                            >
                              <Download size={15} strokeWidth={1.9} aria-hidden="true" />
                            </button>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>
              ) : (
                <div className="cx-session-empty">
                  <History size={22} strokeWidth={1.7} aria-hidden="true" />
                  <span>{sessionQuery ? copy.noMatch : copy.noSessions}</span>
                </div>
              )}
            </div>
          </div>
        </div>

        {diagnostics.length ? (
          <details className="cx-session-diagnostics" open={scanIncomplete || undefined}>
            <summary>
              <AlertCircle size={15} strokeWidth={1.9} aria-hidden="true" />
              <span>{copy.diagnostics}</span>
              <small>{copy.diagnosticsCount(diagnostics.length)}</small>
            </summary>
            <div className="cx-session-diagnostic-items">
              {diagnostics.map((item, index) => <p key={`${index}-${item.message}`}>{item.blocking ? <AlertCircle size={14} aria-hidden="true" /> : <Info size={14} aria-hidden="true" />}{item.message}</p>)}
            </div>
          </details>
        ) : null}
      </section>
    </>
  );
}
