import { useId, useMemo, useState } from "react";
import type { ChangeEvent, RefObject } from "react";
import {
  ArrowLeft,
  ArrowLeftRight,
  CircleHelp,
  CirclePlus,
  FileText,
  Loader2,
  PencilLine,
  Plus,
  RefreshCw,
  Save,
  Settings2,
  Sparkles,
  Trash2,
  Upload,
} from "lucide-react";

import { PageTransition } from "../components/PageTransition";
import { PromptCategoryManager } from "../components/PromptCategoryManager";
import type { PromptCategoryItem } from "../components/PromptCategoryManager";
import { Button, IconButton, StatusBadge, Toggle, cx } from "../components/ui";
import { usePromptCategories } from "../promptCategories";
import type {
  BuiltinPromptStatus,
  InstructionMode,
  InstructionTemplate,
  Lang,
  PromptInjectionMode,
  SavedPrompt,
} from "../types";
import "../styles/skills-prompts-pages.css";

type MaybeAsyncAction = () => void | Promise<void>;

export type ExternalPromptItem = {
  title: string;
  description: string;
  filename?: string | null;
};

export type OrphanedBuiltinPrompt = {
  id: string;
  title: string;
  description?: string | null;
};

export type PromptFormField = "title" | "filename" | "content";

export type PromptsPageProps = {
  lang: Lang;
  instructionMode: InstructionMode;
  promptForm: SavedPrompt;
  editingPromptId: string | null;
  loading: boolean;
  actionBusy: string;
  promptSyncing: boolean;
  promptCatalogReady: boolean;
  promptImportRef: RefObject<HTMLInputElement>;
  promptInjectionMode: PromptInjectionMode;
  promptModeHelpOpen: boolean;
  promptModeHelpRef: RefObject<HTMLDivElement>;
  instructionEnabled: boolean;
  activeInstructionTitle: string;
  activeInjectionMode?: PromptInjectionMode;
  instructionTemplates: InstructionTemplate[];
  builtinPromptStatuses: BuiltinPromptStatus[];
  activeBuiltinTemplateId?: string | null;
  orphanedBuiltinPrompt?: OrphanedBuiltinPrompt | null;
  savedPrompts: SavedPrompt[];
  managedSavedPromptId?: string | null;
  preservedSavedPromptFilename?: string | null;
  externalPrompt?: ExternalPromptItem | null;
  className?: string;
  onSyncBuiltinPrompts: MaybeAsyncAction;
  onImportPrompt: (file?: File | null) => void | Promise<void>;
  onAddPrompt: () => void;
  onInstructionModeChange: (mode: InstructionMode) => void;
  onPromptInjectionModeChange: (mode: PromptInjectionMode) => void;
  onTogglePromptModeHelp: () => void;
  onEnableBuiltinPrompt: (id: string) => void | Promise<void>;
  onDisableInstruction: MaybeAsyncAction;
  onEnableSavedPrompt: (id: string) => void | Promise<void>;
  onDisableExternalPrompt: MaybeAsyncAction;
  onEditPrompt: (prompt: SavedPrompt) => void;
  onDeletePrompt: (id: string) => void | Promise<void>;
  onPromptFormFieldChange: (field: PromptFormField, value: string) => void;
  onSavePrompt: MaybeAsyncAction;
};

function getCopy(lang: Lang) {
  return lang === "zh"
    ? {
        eyebrow: "PROMPT INJECTION",
        title: "一键管理指令提示词",
        description: "选择启用方式，再管理内置、在线或自定义的 Markdown 提示词。",
        sync: "同步 GitHub 模板",
        syncing: "同步中...",
        importMd: "导入 md",
        importing: "导入中...",
        add: "添加提示词",
        manageCategories: "分类管理",
        emptyCategory: "该分类下暂无提示词",
        currentStatus: "当前状态",
        noActive: "未启用提示词",
        keepExisting: "保留原提示词",
        replaceExisting: "替换原提示词",
        appendMode: "追加到 AGENTS.md",
        replaceMode: "替换指令文件",
        appendDetail: "当前模板写入 AGENTS.md，同时保留已有指令文件。",
        replaceDetail: "当前模板通过 model_instructions_file 独立加载。",
        inactiveDetail: "先选择启用方式，再打开下方任一模板。",
        enableMethod: "启用方式",
        helpLabel: "查看启用方式说明",
        appendHelp: "只在 AGENTS.md 中增加 Codex-X 管理区块，不改动原有 model_instructions_file，适合叠加使用。",
        replaceHelp: "当前模板会成为唯一生效的指令入口，原有 model_instructions_file 将被替换。",
        pendingMode: (mode: string) => `当前模式不变，下次启用将使用“${mode}”。`,
        modeHint: "点击模板开关时，使用这里选择的方式。",
        keepTitle: "写入 AGENTS.md，并保留现有 model_instructions_file",
        replaceTitle: "使用 model_instructions_file 替换现有指令文件",
        enable: "启用",
        disable: "关闭",
        disableExternal: "禁用外部提示词",
        current: "当前",
        onlineRemoved: "在线已移除",
        removedDescription: "该模板已从在线目录移除，当前配置仍在使用。",
        customDescription: "自定义指令提示词",
        preservedDescription: "用户原有提示词，追加模式下继续生效。",
        existingPrompt: "用户原有指令提示词",
        edit: "编辑",
        remove: "删除",
        formEyebrow: "CUSTOM PROMPT",
        addFormTitle: "添加提示词",
        editFormTitle: "编辑提示词",
        formDescription: "保存为 Markdown 文件，之后可在列表中单独启用。",
        back: "返回",
        promptDetails: "提示词详情",
        promptName: "提示词名称",
        promptNamePlaceholder: "例如：通用编程助手、代码审查专家",
        filename: "文件名",
        filenamePlaceholder: "my-prompt.md",
        content: "提示词内容",
        contentHint: "Markdown",
        contentPlaceholder: "在此输入提示词内容...",
        save: "保存",
      }
    : {
        eyebrow: "PROMPT INJECTION",
        title: "Manage instruction prompts",
        description: "Choose an activation method, then manage bundled, online, or custom Markdown prompts.",
        sync: "Sync GitHub templates",
        syncing: "Syncing...",
        importMd: "Import md",
        importing: "Importing...",
        add: "Add prompt",
        manageCategories: "Manage categories",
        emptyCategory: "No prompts in this category",
        currentStatus: "Current status",
        noActive: "No prompt enabled",
        keepExisting: "Keep existing",
        replaceExisting: "Replace existing",
        appendMode: "Append to AGENTS.md",
        replaceMode: "Replace instruction file",
        appendDetail: "The current template is written to AGENTS.md while the existing instruction file is preserved.",
        replaceDetail: "The current template is loaded independently through model_instructions_file.",
        inactiveDetail: "Choose an activation method, then turn on a template below.",
        enableMethod: "Enable method",
        helpLabel: "Show activation method help",
        appendHelp: "Adds a Codex-X managed block to AGENTS.md without changing the existing model_instructions_file.",
        replaceHelp: "Makes the selected template the only instruction entry and replaces the existing model_instructions_file.",
        pendingMode: (mode: string) => `The current mode is unchanged. The next enable uses “${mode}”.`,
        modeHint: "This method is used when a template is turned on.",
        keepTitle: "Write to AGENTS.md and preserve model_instructions_file",
        replaceTitle: "Replace the existing instruction file through model_instructions_file",
        enable: "Enable",
        disable: "Disable",
        disableExternal: "Disable external prompt",
        current: "Current",
        onlineRemoved: "Removed online",
        removedDescription: "This template was removed online but is still active.",
        customDescription: "Custom instruction prompt",
        preservedDescription: "Existing user prompt preserved by append mode.",
        existingPrompt: "Existing user prompt",
        edit: "Edit",
        remove: "Delete",
        formEyebrow: "CUSTOM PROMPT",
        addFormTitle: "Add prompt",
        editFormTitle: "Edit prompt",
        formDescription: "Save it as Markdown, then enable it separately from the list.",
        back: "Back",
        promptDetails: "Prompt details",
        promptName: "Prompt name",
        promptNamePlaceholder: "For example: General coding assistant",
        filename: "Filename",
        filenamePlaceholder: "my-prompt.md",
        content: "Prompt content",
        contentHint: "Markdown",
        contentPlaceholder: "Enter prompt content here...",
        save: "Save",
      };
}

function run(action: MaybeAsyncAction) {
  void action();
}

function promptCategoryKey(kind: "builtin" | "saved" | "external", id: string) {
  return `${kind}:${id.trim().toLowerCase()}`;
}

function savedPromptCategoryKey(prompt: SavedPrompt) {
  return prompt.id.startsWith("external-")
    ? promptCategoryKey("external", prompt.filename)
    : promptCategoryKey("saved", prompt.id);
}

type PromptRowProps = {
  title: string;
  description: string;
  enabled: boolean;
  loading: boolean;
  toggleLabel: string;
  onToggle: () => void | Promise<void>;
  children?: React.ReactNode;
  actions?: React.ReactNode;
};

function PromptRow({
  title,
  description,
  enabled,
  loading,
  toggleLabel,
  onToggle,
  children,
  actions,
}: PromptRowProps) {
  return (
    <article className="cx-prompts-row">
      <div className="cx-prompts-row-head">
        <div className="cx-prompts-row-heading">
          <div className="cx-prompts-row-icon" aria-hidden="true"><FileText size={16} strokeWidth={1.9} /></div>
          <div className="cx-prompts-row-title" title={title}><strong>{title}</strong></div>
        </div>
        <div className="cx-prompts-row-actions">
          <Toggle
            checked={enabled}
            onCheckedChange={() => void onToggle()}
            disabled={loading}
            aria-label={toggleLabel}
          />
        </div>
      </div>
      <div className="cx-prompts-row-copy">
        <p title={description}>{description}</p>
      </div>
      {(children || actions) && (
        <div className="cx-prompts-row-footer">
          <div className="cx-prompts-row-details">{children}</div>
          {actions}
        </div>
      )}
    </article>
  );
}

function PromptFormView({
  lang,
  promptForm,
  editingPromptId,
  loading,
  onInstructionModeChange,
  onPromptFormFieldChange,
  onSavePrompt,
}: Pick<
  PromptsPageProps,
  | "lang"
  | "promptForm"
  | "editingPromptId"
  | "loading"
  | "onInstructionModeChange"
  | "onPromptFormFieldChange"
  | "onSavePrompt"
>) {
  const copy = getCopy(lang);
  const titleId = useId();
  const filenameId = useId();
  const contentId = useId();

  return (
    <div className="cx-prompts-form-page">
      <header className="cx-prompts-header cx-prompts-form-header">
        <div className="cx-prompts-heading">
          <p><PencilLine size={14} aria-hidden="true" />{copy.formEyebrow}</p>
          <h2>{editingPromptId ? copy.editFormTitle : copy.addFormTitle}</h2>
          <span>{copy.formDescription}</span>
        </div>
        <Button
          variant="secondary"
          icon={<ArrowLeft />}
          onClick={() => onInstructionModeChange("list")}
          disabled={loading}
        >
          {copy.back}
        </Button>
      </header>

      <section className="cx-prompts-form-panel" aria-labelledby={`${titleId}-panel`}>
        <div className="cx-prompts-form-panel-head">
          <FileText size={18} aria-hidden="true" />
          <h3 id={`${titleId}-panel`}>{copy.promptDetails}</h3>
        </div>
        <div className="cx-prompts-form-grid">
          <label className="cx-prompts-field" htmlFor={titleId}>
            <span>{copy.promptName}</span>
            <input
              id={titleId}
              type="text"
              value={promptForm.title}
              onChange={(event) => onPromptFormFieldChange("title", event.currentTarget.value)}
              placeholder={copy.promptNamePlaceholder}
              disabled={loading}
              autoComplete="off"
            />
          </label>
          <label className="cx-prompts-field" htmlFor={filenameId}>
            <span>{copy.filename}</span>
            <input
              id={filenameId}
              type="text"
              value={promptForm.filename}
              onChange={(event) => onPromptFormFieldChange("filename", event.currentTarget.value)}
              placeholder={copy.filenamePlaceholder}
              disabled={loading}
              autoComplete="off"
              spellCheck={false}
            />
          </label>
          <label className="cx-prompts-field cx-prompts-field--content" htmlFor={contentId}>
            <span>{copy.content}<small>{copy.contentHint}</small></span>
            <textarea
              id={contentId}
              value={promptForm.content}
              onChange={(event) => onPromptFormFieldChange("content", event.currentTarget.value)}
              placeholder={copy.contentPlaceholder}
              disabled={loading}
              spellCheck={false}
            />
          </label>
        </div>
      </section>

      <div className="cx-prompts-form-actions">
        <Button
          size="lg"
          icon={loading ? <Loader2 className="cx-prompts-spin" /> : <Save />}
          onClick={() => run(onSavePrompt)}
          disabled={loading}
        >
          {copy.save}
        </Button>
      </div>
    </div>
  );
}

export function PromptsPage({
  lang,
  instructionMode,
  promptForm,
  editingPromptId,
  loading,
  actionBusy,
  promptSyncing,
  promptCatalogReady,
  promptImportRef,
  promptInjectionMode,
  promptModeHelpOpen,
  promptModeHelpRef,
  instructionEnabled,
  activeInstructionTitle,
  activeInjectionMode,
  instructionTemplates,
  activeBuiltinTemplateId,
  orphanedBuiltinPrompt,
  savedPrompts,
  managedSavedPromptId,
  preservedSavedPromptFilename,
  externalPrompt,
  className,
  onSyncBuiltinPrompts,
  onImportPrompt,
  onAddPrompt,
  onInstructionModeChange,
  onPromptInjectionModeChange,
  onTogglePromptModeHelp,
  onEnableBuiltinPrompt,
  onDisableInstruction,
  onEnableSavedPrompt,
  onDisableExternalPrompt,
  onEditPrompt,
  onDeletePrompt,
  onPromptFormFieldChange,
  onSavePrompt,
}: PromptsPageProps) {
  const copy = getCopy(lang);
  const helpId = useId();
  const selectedModeLabel = promptInjectionMode === "append" ? copy.keepExisting : copy.replaceExisting;
  const activeModeLabel = activeInjectionMode === "append" ? copy.appendMode : copy.replaceMode;
  const modePending = Boolean(instructionEnabled && activeInjectionMode && activeInjectionMode !== promptInjectionMode);
  const importBusy = actionBusy === "importPrompt";
  const [categoryManagerOpen, setCategoryManagerOpen] = useState(false);
  const promptCategories = usePromptCategories(lang);
  const categoryItems = useMemo<PromptCategoryItem[]>(() => [
    ...instructionTemplates.map((template) => ({
      key: promptCategoryKey("builtin", template.id),
      title: template.title,
    })),
    ...(promptCatalogReady && orphanedBuiltinPrompt ? [{
      key: promptCategoryKey("builtin", orphanedBuiltinPrompt.id),
      title: orphanedBuiltinPrompt.title,
    }] : []),
    ...savedPrompts.map((prompt) => ({
      key: savedPromptCategoryKey(prompt),
      title: prompt.title,
    })),
    ...(externalPrompt ? [{
      key: promptCategoryKey("external", externalPrompt.filename || externalPrompt.title),
      title: externalPrompt.title || copy.existingPrompt,
    }] : []),
  ], [copy.existingPrompt, externalPrompt, instructionTemplates, orphanedBuiltinPrompt, promptCatalogReady, savedPrompts]);
  const promptIsVisible = (key: string) =>
    promptCategories.categoryForPrompt(key) === promptCategories.activeCategoryId;
  const visiblePromptCount = categoryItems.filter((item) => promptIsVisible(item.key)).length;
  const deleteSavedPrompt = async (prompt: SavedPrompt) => {
    await onDeletePrompt(prompt.id);
    promptCategories.forgetPrompt(savedPromptCategoryKey(prompt));
  };

  if (instructionMode === "form") {
    return (
      <PageTransition pageKey={`prompts:${instructionMode}`}>
        <section className={cx("cx-prompts-page", "cx-prompts-page--form", className)} aria-label={editingPromptId ? copy.editFormTitle : copy.addFormTitle}>
          <PromptFormView
            lang={lang}
            promptForm={promptForm}
            editingPromptId={editingPromptId}
            loading={loading}
            onInstructionModeChange={onInstructionModeChange}
            onPromptFormFieldChange={onPromptFormFieldChange}
            onSavePrompt={onSavePrompt}
          />
        </section>
      </PageTransition>
    );
  }

  const handlePromptFileChange = (event: ChangeEvent<HTMLInputElement>) => {
    void onImportPrompt(event.currentTarget.files?.[0]);
  };

  return (
    <PageTransition pageKey={`prompts:${instructionMode}`}>
      <section className={cx("cx-prompts-page", "cx-prompts-page--list", className)} aria-label={copy.title}>
      <PromptCategoryManager
        open={categoryManagerOpen}
        lang={lang}
        categories={promptCategories.categories}
        prompts={categoryItems}
        categoryForPrompt={promptCategories.categoryForPrompt}
        onClose={() => setCategoryManagerOpen(false)}
        onAddCategory={promptCategories.addCategory}
        onRenameCategory={promptCategories.renameCategory}
        onDeleteCategory={promptCategories.deleteCategory}
        onMovePrompt={promptCategories.movePrompt}
      />
      <header className="cx-prompts-header">
        <div className="cx-prompts-heading">
          <p><Sparkles size={14} aria-hidden="true" />{copy.eyebrow}</p>
          <h2>{copy.title}</h2>
        </div>
        <div className="cx-prompts-header-actions">
          <input
            ref={promptImportRef}
            className="cx-prompts-file-input"
            type="file"
            accept=".md,text/markdown,text/plain"
            onChange={handlePromptFileChange}
            disabled={loading}
            tabIndex={-1}
            aria-hidden="true"
          />
          <Button
            variant="secondary"
            icon={promptSyncing ? <Loader2 className="cx-prompts-spin" /> : <RefreshCw />}
            onClick={() => run(onSyncBuiltinPrompts)}
            disabled={loading || promptSyncing}
          >
            {promptSyncing ? copy.syncing : copy.sync}
          </Button>
          <Button
            variant="secondary"
            icon={importBusy ? <Loader2 className="cx-prompts-spin" /> : <Upload />}
            onClick={() => promptImportRef.current?.click()}
            disabled={loading}
          >
            {importBusy ? copy.importing : copy.importMd}
          </Button>
          <Button className="cx-prompts-add-button" icon={<Plus />} onClick={onAddPrompt} disabled={loading}>
            {copy.add}
          </Button>
        </div>
      </header>

      <section className="cx-prompts-mode-panel">
        <div className="cx-prompts-active-summary">
          <p>{copy.currentStatus}</p>
          <div className="cx-prompts-active-title" aria-live="polite">
            <span className={cx("cx-prompts-state-dot", instructionEnabled && "cx-prompts-state-dot--active")} aria-hidden="true" />
            <strong>{instructionEnabled ? activeInstructionTitle : copy.noActive}</strong>
            {instructionEnabled && <StatusBadge tone="success" dot={false}>{activeModeLabel}</StatusBadge>}
          </div>
          <span>
            {instructionEnabled
              ? activeInjectionMode === "append" ? copy.appendDetail : copy.replaceDetail
              : copy.inactiveDetail}
          </span>
        </div>

        <div className="cx-prompts-mode-choice">
          <div className="cx-prompts-mode-copy" ref={promptModeHelpRef}>
            <div className="cx-prompts-mode-title">
              <strong>{copy.enableMethod}</strong>
              <IconButton
                icon={<CircleHelp size={15} />}
                label={copy.helpLabel}
                variant="ghost"
                size="sm"
                aria-expanded={promptModeHelpOpen}
                aria-controls={helpId}
                onClick={onTogglePromptModeHelp}
              />
            </div>
            {promptModeHelpOpen && (
              <div id={helpId} className="cx-prompts-mode-help" role="dialog" aria-label={copy.helpLabel}>
                <div><strong>{copy.keepExisting}</strong><span>{copy.appendHelp}</span></div>
                <div><strong>{copy.replaceExisting}</strong><span>{copy.replaceHelp}</span></div>
              </div>
            )}
            <span>{modePending ? copy.pendingMode(selectedModeLabel) : copy.modeHint}</span>
          </div>
          <div className="cx-prompts-mode-segments" role="radiogroup" aria-label={copy.enableMethod}>
            <button
              type="button"
              role="radio"
              aria-checked={promptInjectionMode === "append"}
              className={cx("cx-prompts-mode-button", promptInjectionMode === "append" && "cx-prompts-mode-button--active")}
              title={copy.keepTitle}
              onClick={() => onPromptInjectionModeChange("append")}
            >
              <CirclePlus size={16} aria-hidden="true" />
              {copy.keepExisting}
            </button>
            <button
              type="button"
              role="radio"
              aria-checked={promptInjectionMode === "replace"}
              className={cx("cx-prompts-mode-button", promptInjectionMode === "replace" && "cx-prompts-mode-button--active")}
              title={copy.replaceTitle}
              onClick={() => onPromptInjectionModeChange("replace")}
            >
              <ArrowLeftRight size={16} aria-hidden="true" />
              {copy.replaceExisting}
            </button>
          </div>
        </div>
      </section>

      <div className="cx-prompt-category-toolbar">
        <div className="cx-skills-tabs cx-prompt-category-tabs" role="group" aria-label={copy.manageCategories}>
          {promptCategories.categories.map((category) => (
            <button
              type="button"
              aria-pressed={category.id === promptCategories.activeCategoryId}
              className={cx("cx-skills-tab", "cx-prompt-category-tab", category.id === promptCategories.activeCategoryId && "cx-skills-tab--active")}
              key={category.id}
              onClick={() => promptCategories.setActiveCategoryId(category.id)}
            >
              {category.name}
            </button>
          ))}
        </div>
        <button type="button" className="cx-prompt-category-manage" onClick={() => setCategoryManagerOpen(true)}>
          <Settings2 size={16} aria-hidden="true" />{copy.manageCategories}
        </button>
      </div>

      <PageTransition pageKey={`prompts-category:${promptCategories.activeCategoryId}`}>
        <section className="cx-prompts-list-panel" aria-label={lang === "zh" ? "提示词模板" : "Prompt templates"}>
          <div className="cx-prompts-list">
          {visiblePromptCount === 0 && (
            <div className="cx-prompt-category-list-empty"><FileText size={22} aria-hidden="true" />{copy.emptyCategory}</div>
          )}
          {instructionTemplates.filter((template) =>
            promptIsVisible(promptCategoryKey("builtin", template.id))).map((template) => {
            const enabled = template.id === activeBuiltinTemplateId;
            return (
              <PromptRow
                key={template.id}
                title={template.title}
                description={template.subtitle}
                enabled={enabled}
                loading={loading}
                toggleLabel={enabled ? copy.disable : copy.enable}
                onToggle={() => enabled ? onDisableInstruction() : onEnableBuiltinPrompt(template.id)}
              >
                {enabled && (
                  <div className="cx-prompts-row-meta">
                    <StatusBadge tone="accent" dot={false}>{copy.current} · {activeModeLabel}</StatusBadge>
                  </div>
                )}
              </PromptRow>
            );
          })}

          {promptCatalogReady && orphanedBuiltinPrompt
            && promptIsVisible(promptCategoryKey("builtin", orphanedBuiltinPrompt.id)) && (
            <PromptRow
              title={orphanedBuiltinPrompt.title}
              description={orphanedBuiltinPrompt.description || copy.removedDescription}
              enabled
              loading={loading}
              toggleLabel={copy.disable}
              onToggle={onDisableInstruction}
            >
              <div className="cx-prompts-row-meta">
                <StatusBadge tone="accent" dot={false}>{copy.current} · {activeModeLabel}</StatusBadge>
                <StatusBadge tone="warning" dot={false}>{copy.onlineRemoved}</StatusBadge>
              </div>
            </PromptRow>
          )}

          {savedPrompts.filter((prompt) =>
            promptIsVisible(savedPromptCategoryKey(prompt))).map((prompt) => {
            const managed = prompt.id === managedSavedPromptId;
            const preserved = !managed && Boolean(preservedSavedPromptFilename) && prompt.filename === preservedSavedPromptFilename;
            const enabled = managed || preserved;
            return (
              <PromptRow
                key={prompt.id}
                title={prompt.title}
                description={preserved ? copy.preservedDescription : copy.customDescription}
                enabled={enabled}
                loading={loading}
                toggleLabel={managed ? copy.disable : preserved ? copy.disableExternal : copy.enable}
                onToggle={() => managed
                  ? onDisableInstruction()
                  : preserved
                    ? onDisableExternalPrompt()
                    : onEnableSavedPrompt(prompt.id)}
                actions={(
                  <div className="cx-prompts-icon-actions">
                    <IconButton
                      icon={<PencilLine size={15} />}
                      label={copy.edit}
                      size="sm"
                      onClick={() => onEditPrompt(prompt)}
                      disabled={loading}
                    />
                    <IconButton
                      icon={<Trash2 size={15} />}
                      label={copy.remove}
                      variant="danger"
                      size="sm"
                      onClick={() => void deleteSavedPrompt(prompt)}
                      disabled={loading}
                    />
                  </div>
                )}
              >
                {managed && (
                  <div className="cx-prompts-row-meta">
                    <StatusBadge tone="accent" dot={false}>{copy.current} · {activeModeLabel}</StatusBadge>
                  </div>
                )}
                {!managed && preserved && (
                  <div className="cx-prompts-row-meta">
                    <StatusBadge tone="info" dot={false}>{copy.current} · {copy.appendMode}</StatusBadge>
                  </div>
                )}
              </PromptRow>
            );
          })}

          {externalPrompt
            && promptIsVisible(promptCategoryKey("external", externalPrompt.filename || externalPrompt.title)) && (
            <PromptRow
              title={externalPrompt.title || copy.existingPrompt}
              description={externalPrompt.description}
              enabled
              loading={loading}
              toggleLabel={copy.disableExternal}
              onToggle={onDisableExternalPrompt}
            >
              {externalPrompt.filename && <code className="cx-prompts-external-path">{externalPrompt.filename}</code>}
            </PromptRow>
          )}
          </div>
        </section>
      </PageTransition>
      </section>
    </PageTransition>
  );
}
