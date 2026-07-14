import React from "react";
import {
  AlertCircle,
  CheckCircle2,
  History,
  Info,
  Loader2,
  RefreshCw,
  Search,
  Trash2,
  Zap,
} from "lucide-react";

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
  sessionDeleteDialogRef: React.RefObject<HTMLDivElement>;
  sessionDeleteTriggerRef: React.RefObject<HTMLButtonElement>;
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

function cx(...items: Array<string | false | undefined>) {
  return items.filter(Boolean).join(" ");
}

function formatSessionTime(value?: number | null) {
  if (!value) return "未知时间";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "未知时间";
  return date.toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function compactPath(value?: string | null, max = 58) {
  if (!value) return "未记录路径";
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
  sessionDeleteDialogRef,
  sessionDeleteTriggerRef,
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
  return (
    <>
      {sessionDeleteConfirmOpen && selectedSessions.length > 0 && (
        <div className="update-mask" onClick={onCloseDeleteConfirm}>
          <div
            ref={sessionDeleteDialogRef}
            className="update-dialog glass session-delete-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="session-delete-title"
            aria-describedby="session-delete-description"
            tabIndex={-1}
            onClick={(event) => event.stopPropagation()}
          >
            <div className="update-head session-delete-head">
              <div className="update-icon session-delete-icon"><Trash2 size={22} /></div>
              <div>
                <p className="eyebrow">Codex Storage</p>
                <h3 id="session-delete-title">
                  {lang === "zh" ? `永久删除 ${selectedSessions.length} 条会话` : `Permanently delete ${selectedSessions.length} session(s)`}
                </h3>
              </div>
            </div>
            <div className="update-body session-delete-body">
              <div className="session-delete-warning">
                <AlertCircle size={19} />
                <div>
                  <strong>{lang === "zh" ? "此操作不可恢复" : "This cannot be undone"}</strong>
                  <p id="session-delete-description">
                    {lang === "zh"
                      ? "所选会话将从当前 Codex 存储中硬删除，不会移入回收站，也不会创建新的备份；Codex-X 无法撤销或恢复。"
                      : "The selected sessions will be hard-deleted from the current Codex storage. No recycle bin or new backup will be created, and Codex-X cannot undo or restore them."}
                  </p>
                  <p>
                    {lang === "zh"
                      ? "由这些会话派生的子会话也会一并删除。"
                      : "Child sessions spawned from these sessions will also be deleted."}
                  </p>
                  <p>
                    {lang === "zh"
                      ? "删除前请关闭其他正在使用这些会话的 Codex 窗口或 CLI；活动会话仍可能继续写入并导致删除失败。"
                      : "Before deleting, close other Codex windows or CLIs using these sessions. An active session may keep writing and cause deletion to fail."}
                  </p>
                </div>
              </div>
              <div className="session-delete-list" aria-label={lang === "zh" ? "待删除会话" : "Sessions to delete"}>
                {selectedSessions.slice(0, 8).map((item) => (
                  <div className="session-delete-item" key={item.id}>
                    <strong title={item.title}>{item.title || (lang === "zh" ? "未命名会话" : "Untitled session")}</strong>
                    <code>#{shortId(item.id)}</code>
                    <span title={item.cwd || item.rolloutPath || undefined}>{compactPath(item.cwd || item.rolloutPath, 72)}</span>
                  </div>
                ))}
                {selectedSessions.length > 8 && (
                  <p className="session-delete-more">
                    {lang === "zh" ? `另有 ${selectedSessions.length - 8} 条会话未在此处展开` : `${selectedSessions.length - 8} more session(s) not shown`}
                  </p>
                )}
              </div>
              <label className="session-delete-process-check">
                <input
                  type="checkbox"
                  checked={sessionDeleteSafetyConfirmed}
                  onChange={(event) => onDeleteSafetyConfirmedChange(event.target.checked)}
                  disabled={sessionDeleteBusy}
                />
                <span>
                  {lang === "zh"
                    ? "我已关闭其他正在使用这些会话的 Codex 窗口或 CLI"
                    : "I closed other Codex windows or CLIs using these sessions"}
                </span>
              </label>
            </div>
            <div className="update-actions session-delete-actions">
              <button className="secondary-btn" onClick={onCloseDeleteConfirm} disabled={sessionDeleteBusy} data-initial-focus>
                {lang === "zh" ? "取消" : "Cancel"}
              </button>
              <button className="danger-btn session-delete-confirm-btn" onClick={onDeleteSelectedSessions} disabled={sessionDeleteBusy || !sessionDeleteSafetyConfirmed}>
                {sessionDeleteBusy ? <Loader2 size={17} className="spin" /> : <Trash2 size={17} />}
                {sessionDeleteBusy
                  ? (lang === "zh" ? "正在永久删除..." : "Deleting permanently...")
                  : (lang === "zh" ? `确认永久删除 ${selectedSessions.length} 条` : `Delete ${selectedSessions.length} permanently`)}
              </button>
            </div>
          </div>
        </div>
      )}

      <section className={cx("panel glass sessions-panel", !active && "page-pane-hidden")}>
        <div className="panel-head provider-title-row session-page-head">
          <div>
            <p className="eyebrow">{lang === "zh" ? "会话同步" : "Session sync"}</p>
            <h3>{lang === "zh" ? "会话管理" : "Session management"}</h3>
            <p className="muted-desc">
              {lang === "zh"
                ? "检查本地会话是否跟当前供应商一致，需要时一键同步。不会修改聊天内容。"
                : "Check whether local sessions match the current provider and sync them with one click when needed. Chat content is not changed."}
            </p>
          </div>
          <div className="provider-title-actions session-title-actions">
            <span className="session-provider-chip">
              {lang === "zh" ? "同步到" : "Sync to"}: {sessionTargetLabel}
            </span>
            <button className="secondary-btn add-provider-btn lively-btn" onClick={onCheckSessions} disabled={loading}>
              {actionBusy === "checkSessions" ? <Loader2 size={18} className="spin" /> : <RefreshCw size={18} />} {actionBusy === "checkSessions" ? (lang === "zh" ? "检查中..." : "Checking...") : (lang === "zh" ? "检查会话" : "Check")}
            </button>
            <button className="primary-btn add-provider-btn lively-btn" onClick={onSyncSessions} disabled={loading || !sessionHasMismatches}>
              {actionBusy === "syncSessions" ? <Loader2 size={18} className="spin" /> : <Zap size={18} />} {actionBusy === "syncSessions" ? (lang === "zh" ? "同步中..." : "Syncing...") : (lang === "zh" ? "同步会话" : "Sync sessions")}
            </button>
          </div>
        </div>

        <div className={cx("session-compact-summary", sessionHasMismatches ? "needs-sync" : "synced")}>
          <span className="session-summary-status">
            {!sessionStatus
              ? <Info size={15} />
              : sessionHasMismatches
                ? <AlertCircle size={15} />
                : <CheckCircle2 size={15} />}
            {!sessionStatus
              ? (lang === "zh" ? "点击检查会话" : "Check sessions to get started")
              : sessionHasMismatches
                ? (lang === "zh" ? `有 ${sessionSyncCount} 条会话需要同步` : `${sessionSyncCount} session(s) need syncing`)
                : (lang === "zh" ? "全部会话已同步" : "All sessions are synced")}
          </span>
          <span>{lang === "zh" ? `${sessionStatus?.topLevelThreads ?? 0} 条会话` : `${sessionStatus?.topLevelThreads ?? 0} sessions`}</span>
        </div>

        <div className="session-list-card">
          <div className="session-list-head session-list-head-rich">
            <div>
              <p className="eyebrow">{lang === "zh" ? "本地会话" : "Local threads"}</p>
              <h4>{lang === "zh" ? "会话列表" : "Sessions"}</h4>
            </div>
            <span title={sessionPreviewTruncated ? (lang === "zh" ? `当前加载 ${visibleSessions.length} 条` : `${visibleSessions.length} loaded`) : undefined}>
              {lang === "zh" ? `展示 ${filteredSessions.length} / ${sessionVisibleTotal} 条` : `${filteredSessions.length} / ${sessionVisibleTotal} shown`}
            </span>
          </div>

          <div className="session-toolbar">
            <label className="session-search">
              <Search size={16} />
              <input
                value={sessionQuery}
                onChange={(event) => onSessionQueryChange(event.target.value)}
                placeholder={lang === "zh" ? "搜索标题 / 项目 / 供应商 / ID" : "Search title / project / provider / ID"}
              />
            </label>
            <label className="session-toggle">
              <input type="checkbox" checked={sessionGroupByCwd} onChange={(event) => onSessionGroupByCwdChange(event.target.checked)} />
              <span>{lang === "zh" ? "按项目路径分组" : "Group by cwd"}</span>
            </label>
            {(sessionStatus?.subagentThreads ?? 0) > 0 && (
              <label className="session-toggle session-internal-toggle">
                <input
                  type="checkbox"
                  checked={showInternalSessions}
                  onChange={(event) => onShowInternalSessionsChange(event.target.checked)}
                />
                <span>{lang === "zh"
                  ? `显示内部会话 (${sessionStatus?.subagentThreads ?? 0})`
                  : `Show internal sessions (${sessionStatus?.subagentThreads ?? 0})`}</span>
              </label>
            )}
            <button
              ref={sessionDeleteTriggerRef}
              className={cx("small session-delete-trigger", selectedSessionIds.length > 0 ? "danger-btn active" : "secondary-btn")}
              onClick={onOpenDeleteConfirm}
              disabled={loading || sessionDeleteBusy || selectedSessionIds.length === 0}
              title={selectedSessionIds.length > 0 ? undefined : (lang === "zh" ? "先勾选要删除的会话" : "Select sessions to delete")}
            >
              <Trash2 size={16} />
              {selectedSessionIds.length > 0
                ? (lang === "zh" ? `永久删除 ${selectedSessionIds.length} 条` : `Delete ${selectedSessionIds.length} permanently`)
                : (lang === "zh" ? "删除选中" : "Delete selected")}
            </button>
          </div>

          {filteredSessions.length ? (
            <div className="session-list enhanced-session-list">
              <div className="session-column-head">
                <span aria-hidden="true" />
                <span>{lang === "zh" ? "会话" : "Session"}</span>
                <span>{lang === "zh" ? "更新时间" : "Updated"}</span>
                <span>{lang === "zh" ? "供应商" : "Provider"}</span>
                <span>{lang === "zh" ? "模型" : "Model"}</span>
                <span>ID</span>
              </div>
              {groupedSessions.map(([group, items]) => {
                const showGroupHeader = sessionGroupByCwd;
                const projectSessions = allSessionsByCwd.get(group) || items;
                const selectedProjectCount = projectSessions.filter((item) => selectedSessionSet.has(item.id)).length;
                const projectSelected = selectedProjectCount === projectSessions.length;
                const projectPartiallySelected = selectedProjectCount > 0 && !projectSelected;
                const groupCountLabel = items.length === projectSessions.length
                  ? (lang === "zh" ? `${sessionPreviewTruncated ? "已加载 " : ""}${projectSessions.length} 条` : `${projectSessions.length}${sessionPreviewTruncated ? " loaded" : ""}`)
                  : (lang === "zh" ? `显示 ${items.length} / 共 ${projectSessions.length} 条` : `${items.length} / ${projectSessions.length} shown`);
                return (
                  <div className="session-group" key={group}>
                    {showGroupHeader && (
                      <label className={cx("session-group-title", projectSelected && "selected", projectPartiallySelected && "partial")}>
                        <input
                          className="session-checkbox"
                          type="checkbox"
                          ref={(input) => {
                            if (input) input.indeterminate = projectPartiallySelected;
                          }}
                          checked={projectSelected}
                          onChange={(event) => onSetSessionGroupSelected(projectSessions, event.target.checked)}
                          aria-label={lang === "zh" ? `选择当前列表中项目 ${group} 的 ${projectSessions.length} 条会话` : `Select ${projectSessions.length} loaded sessions in project ${group}`}
                        />
                        <span title={group}>{compactPath(group, 96)}</span>
                        <em>{groupCountLabel}</em>
                      </label>
                    )}
                    {items.map((item) => (
                      <label
                        className={cx("session-row", item.needsSync && "needs-sync", selectedSessionSet.has(item.id) && "selected")}
                        key={item.id}
                      >
                        <span className="session-select-box" title={lang === "zh" ? "选择这个会话" : "Select this session"}>
                          <input
                            className="session-checkbox"
                            type="checkbox"
                            checked={selectedSessionSet.has(item.id)}
                            onChange={() => onToggleSessionSelected(item.id)}
                            aria-label={`${lang === "zh" ? "选择会话" : "Select session"}: ${item.title} (#${shortId(item.id)})`}
                          />
                        </span>
                        <div className="session-row-text">
                          <div className="session-row-title">
                            <strong>{item.title}</strong>
                            {item.archived && <span className="session-state-text">{lang === "zh" ? "已归档" : "Archived"}</span>}
                            {item.isSubagent && <span className="mini-tag">{lang === "zh" ? "内部" : "Internal"}</span>}
                            {item.needsSync && (
                              <span className="session-state-text warn">
                                {lang === "zh" ? "待同步" : "Needs sync"}
                              </span>
                            )}
                          </div>
                          {!showGroupHeader && <p title={item.cwd || item.rolloutPath || undefined}>{compactPath(item.cwd || item.rolloutPath, 72)}</p>}
                        </div>
                        <span className="session-meta-time" title={item.updatedAtMs ? new Date(item.updatedAtMs).toLocaleString() : undefined}>{formatSessionTime(item.updatedAtMs)}</span>
                        <code className="session-meta-provider" title={item.modelProvider || undefined}>
                          {item.modelProvider || "unknown"}
                        </code>
                        <span className="session-meta-model" title={item.model || undefined}>{item.model || "-"}</span>
                        <small className="session-meta-id" title={item.id}>#{shortId(item.id)}</small>
                      </label>
                    ))}
                  </div>
                );
              })}
            </div>
          ) : (
            <div className="session-empty">
              <History size={22} />
              <span>{sessionQuery ? (lang === "zh" ? "没有匹配的会话。" : "No matching sessions.") : (lang === "zh" ? "还没有读取到会话。点击右上角“检查会话”刷新。" : "No sessions loaded. Click Check to refresh.")}</span>
            </div>
          )}
        </div>

        {sessionStatus?.warnings?.length ? (
          <details className="session-warning-list session-scan-warnings">
            <summary>
              <Info size={15} />
              <span>{lang === "zh" ? "诊断信息" : "Diagnostics"}</span>
              <small>{lang === "zh" ? `${sessionStatus.warnings.length} 条 · 点击查看` : `${sessionStatus.warnings.length} · click to view`}</small>
            </summary>
            <div className="session-warning-items">
              {sessionStatus.warnings.map((item, index) => <p key={index}><Info size={15} /> {item}</p>)}
            </div>
          </details>
        ) : null}
      </section>
    </>
  );
}
