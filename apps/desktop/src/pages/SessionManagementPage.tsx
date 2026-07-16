import {
  AlertCircle,
  CheckCircle2,
  FolderTree,
  History,
  Info,
  Loader2,
  RefreshCw,
  Search,
  Trash2,
  Zap,
} from "lucide-react";
import { Button, Checkbox, ModalShell, cx } from "../components/ui";
import "../styles/session-management.css";

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
  needsSync: boolean;
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
  onDeleteSafetyConfirmedChange,
}: SessionManagementPageProps) {
  const isChinese = lang === "zh";
  const copy = isChinese
    ? {
        syncEyebrow: "会话同步",
        title: "会话管理",
        description: "检查本地会话是否跟当前供应商一致，需要时一键同步。不会修改聊天内容。",
        syncTo: "同步到",
        check: "检查会话",
        checking: "检查中...",
        sync: "同步会话",
        syncing: "同步中...",
        clickToCheck: "点击检查会话",
        needsSync: (count: number) => `有 ${count} 条会话需要同步`,
        allSynced: "全部会话已同步",
        sessionCount: (count: number) => `${count} 条会话`,
        local: "本地会话",
        list: "会话列表",
        shown: (shown: number, total: number) => `展示 ${shown} / ${total} 条`,
        loaded: (count: number) => `当前加载 ${count} 条`,
        search: "搜索标题 / 项目 / 供应商 / ID",
        groupByProject: "按项目路径分组",
        showInternal: (count: number) => `显示内部会话 (${count})`,
        deleteSelected: "删除选中",
        deleteMany: (count: number) => `永久删除 ${count} 条`,
        selectAll: "选择当前列表中的全部会话",
        selectProject: (path: string, count: number) => `选择项目 ${path} 的 ${count} 条会话`,
        projectCount: (count: number, truncated: boolean) => `${truncated ? "已加载 " : ""}${count} 条`,
        projectShown: (shown: number, total: number) => `显示 ${shown} / 共 ${total} 条`,
        selectSession: "选择会话",
        archived: "已归档",
        internal: "内部",
        pending: "待同步",
        unknownProvider: "未知供应商",
        noModel: "未记录",
        noMatch: "没有匹配的会话。",
        noSessions: "还没有读取到会话。点击右上角“检查会话”刷新。",
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
        description: "Check whether local sessions match the current provider and sync them when needed. Chat content is not changed.",
        syncTo: "Sync to",
        check: "Check sessions",
        checking: "Checking...",
        sync: "Sync sessions",
        syncing: "Syncing...",
        clickToCheck: "Check sessions to get started",
        needsSync: (count: number) => `${count} session(s) need syncing`,
        allSynced: "All sessions are synced",
        sessionCount: (count: number) => `${count} sessions`,
        local: "LOCAL SESSIONS",
        list: "Sessions",
        shown: (shown: number, total: number) => `${shown} / ${total} shown`,
        loaded: (count: number) => `${count} loaded`,
        search: "Search title / project / provider / ID",
        groupByProject: "Group by project path",
        showInternal: (count: number) => `Show internal sessions (${count})`,
        deleteSelected: "Delete selected",
        deleteMany: (count: number) => `Delete ${count} permanently`,
        selectAll: "Select all sessions in the current list",
        selectProject: (path: string, count: number) => `Select ${count} sessions in ${path}`,
        projectCount: (count: number, truncated: boolean) => `${count}${truncated ? " loaded" : ""}`,
        projectShown: (shown: number, total: number) => `${shown} / ${total} shown`,
        selectSession: "Select session",
        archived: "Archived",
        internal: "Internal",
        pending: "Needs sync",
        unknownProvider: "Unknown provider",
        noModel: "Not recorded",
        noMatch: "No matching sessions.",
        noSessions: "No sessions loaded. Click Check sessions to refresh.",
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

  const dialogOpen = sessionDeleteConfirmOpen && selectedSessions.length > 0;
  const selectedVisibleCount = filteredSessions.filter((item) => selectedSessionSet.has(item.id)).length;
  const allVisibleSelected = filteredSessions.length > 0 && selectedVisibleCount === filteredSessions.length;
  const visibleSelectionIsPartial = selectedVisibleCount > 0 && !allVisibleSelected;

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
            <button type="button" className="cx-session-button cx-session-button--primary" onClick={onSyncSessions} disabled={loading || !sessionHasMismatches} aria-busy={actionBusy === "syncSessions"}>
              {actionBusy === "syncSessions" ? <Loader2 size={16} className="cx-session-spin" aria-hidden="true" /> : <Zap size={16} aria-hidden="true" />}
              {actionBusy === "syncSessions" ? copy.syncing : copy.sync}
            </button>
          </div>
        </header>

        <div className={cx("cx-session-summary", sessionHasMismatches ? "cx-session-summary--needs-sync" : "cx-session-summary--synced")}>
          <span className="cx-session-summary-status">
            {!sessionStatus ? <Info size={15} aria-hidden="true" /> : sessionHasMismatches ? <AlertCircle size={15} aria-hidden="true" /> : <CheckCircle2 size={15} aria-hidden="true" />}
            {!sessionStatus ? copy.clickToCheck : sessionHasMismatches ? copy.needsSync(sessionSyncCount) : copy.allSynced}
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
                label={copy.showInternal(sessionStatus?.subagentThreads ?? 0)}
              />
            )}
            <button
              type="button"
              className={cx("cx-session-button cx-session-delete-trigger", selectedSessionIds.length > 0 ? "cx-session-button--danger" : "cx-session-button--secondary")}
              onClick={onOpenDeleteConfirm}
              disabled={loading || sessionDeleteBusy || selectedSessionIds.length === 0}
              title={selectedSessionIds.length > 0 ? undefined : copy.deleteSelected}
            >
              <Trash2 size={15} strokeWidth={1.9} aria-hidden="true" />
              {selectedSessionIds.length > 0 ? copy.deleteMany(selectedSessionIds.length) : copy.deleteSelected}
            </button>
          </div>

          {filteredSessions.length > 0 ? (
            <div className="cx-session-scroll" role="table" aria-label={copy.list}>
              <div className="cx-session-column-head" role="row">
                <Checkbox
                  className="cx-session-select-all"
                  checked={allVisibleSelected}
                  indeterminate={visibleSelectionIsPartial}
                  onCheckedChange={(checked) => onSetSessionGroupSelected(filteredSessions, checked)}
                  aria-label={copy.selectAll}
                  disabled={loading || sessionDeleteBusy}
                />
                <span>{isChinese ? "会话" : "Session"}</span>
                <span>{isChinese ? "更新时间" : "Updated"}</span>
                <span>{isChinese ? "供应商" : "Provider"}</span>
                <span>{isChinese ? "模型" : "Model"}</span>
                <span>ID</span>
              </div>
              <div className="cx-session-table-body">
                {groupedSessions.map(([group, items]) => {
                  const projectSessions = allSessionsByCwd.get(group) || items;
                  const selectedProjectCount = projectSessions.filter((item) => selectedSessionSet.has(item.id)).length;
                  const projectSelected = projectSessions.length > 0 && selectedProjectCount === projectSessions.length;
                  const projectPartiallySelected = selectedProjectCount > 0 && !projectSelected;
                  const groupCountLabel = items.length === projectSessions.length
                    ? copy.projectCount(projectSessions.length, sessionPreviewTruncated)
                    : copy.projectShown(items.length, projectSessions.length);
                  return (
                    <div className="cx-session-group" key={group}>
                      {sessionGroupByCwd && (
                        <label className={cx("cx-session-group-heading", projectSelected && "cx-session-group-heading--selected", projectPartiallySelected && "cx-session-group-heading--partial")}>
                          <input
                            className="cx-session-checkbox"
                            type="checkbox"
                            ref={(input) => {
                              if (input) input.indeterminate = projectPartiallySelected;
                            }}
                            checked={projectSelected}
                            onChange={(event) => onSetSessionGroupSelected(projectSessions, event.target.checked)}
                            aria-label={copy.selectProject(group, projectSessions.length)}
                          />
                          <span title={group}>{compactPath(group, 96, isChinese ? "未记录路径" : "No path recorded")}</span>
                          <em>{groupCountLabel}</em>
                        </label>
                      )}
                      {items.map((item) => (
                        <label className={cx("cx-session-row", item.needsSync && "cx-session-row--needs-sync", selectedSessionSet.has(item.id) && "cx-session-row--selected")} key={item.id}>
                          <span className="cx-session-select-box" title={copy.selectSession}>
                            <input
                              className="cx-session-checkbox"
                              type="checkbox"
                              checked={selectedSessionSet.has(item.id)}
                              onChange={() => onToggleSessionSelected(item.id)}
                              aria-label={`${copy.selectSession}: ${item.title || (isChinese ? "未命名会话" : "Untitled session")} (#${shortId(item.id)})`}
                            />
                          </span>
                          <div className="cx-session-row-copy">
                            <div className="cx-session-row-title">
                              <strong title={item.title}>{item.title || (isChinese ? "未命名会话" : "Untitled session")}</strong>
                              {item.archived && <span className="cx-session-state">{copy.archived}</span>}
                              {item.isSubagent && <span className="cx-session-state">{copy.internal}</span>}
                              {item.needsSync && <span className="cx-session-state cx-session-state--warn">{copy.pending}</span>}
                            </div>
                            {!sessionGroupByCwd && <p title={item.cwd || item.rolloutPath || undefined}>{compactPath(item.cwd || item.rolloutPath, 72, isChinese ? "未记录路径" : "No path recorded")}</p>}
                          </div>
                          <span className="cx-session-meta cx-session-meta--time" title={item.updatedAtMs ? new Date(item.updatedAtMs).toLocaleString() : undefined}>{formatSessionTime(item.updatedAtMs, lang)}</span>
                          <code className="cx-session-meta cx-session-meta--provider" title={item.modelProvider || undefined}>{item.modelProvider || copy.unknownProvider}</code>
                          <span className="cx-session-meta cx-session-meta--model" title={item.model || undefined}>{item.model || copy.noModel}</span>
                          <small className="cx-session-meta cx-session-meta--id" title={item.id}>#{shortId(item.id)}</small>
                        </label>
                      ))}
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

        {sessionStatus?.warnings?.length ? (
          <details className="cx-session-diagnostics">
            <summary>
              <AlertCircle size={15} strokeWidth={1.9} aria-hidden="true" />
              <span>{copy.diagnostics}</span>
              <small>{copy.diagnosticsCount(sessionStatus.warnings.length)}</small>
            </summary>
            <div className="cx-session-diagnostic-items">
              {sessionStatus.warnings.map((item, index) => <p key={`${index}-${item}`}><Info size={14} aria-hidden="true" />{item}</p>)}
            </div>
          </details>
        ) : null}
      </section>
    </>
  );
}
