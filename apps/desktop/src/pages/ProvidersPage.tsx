import { useId, useState } from "react";
import type { ReactNode } from "react";
import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  Eye,
  EyeOff,
  Loader2,
  PencilLine,
  Plus,
  RefreshCw,
  Trash2,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { Ref } from "react";
import { PageTransition } from "../components/PageTransition";
import { Button, Checkbox, ModalShell } from "../components/ui";
import type { ProviderMode } from "../types";
import "../styles/providers-page.css";

export type ProviderRowSource = "official" | "local" | "detected";

export type ProviderRow = {
  id: string;
  source: ProviderRowSource;
  providerName: string;
  baseUrl: string;
  model: string;
  apiKey?: string;
  wireApi: string;
  requiresOpenaiAuth: boolean;
  isCurrent: boolean;
  sourceLabel?: string;
  editable?: boolean;
  deletable?: boolean;
  testable?: boolean;
  testingKey?: string;
  meta?: ReactNode;
};

export type ProviderFormValue = {
  apiKey: string;
  baseUrl: string;
  providerName: string;
  model: string;
  wireApi: string;
  requiresOpenaiAuth: boolean;
};

export type OfficialFormValue = {
  model: string;
  authJson: string;
};

export type ProviderCopy = {
  eyebrow: string;
  title: string;
  subtitle: string;
  importLabel: string;
  addLabel: string;
  noProviders: string;
  currentLabel: string;
  enableLabel: string;
  testLabel: string;
  editLabel: string;
  removeLabel: string;
  deleteTitle: string;
  deleteDescription: (providerName: string) => string;
  deleteCurrentDescription: (providerName: string) => string;
  deleteCancelLabel: string;
  deleteConfirmLabel: string;
  noBaseUrlLabel: string;
  officialEyebrow: string;
  officialTitle: string;
  officialHint: string;
  officialUrlLabel: string;
  authPathLabel: string;
  officialCurrentLabel: string;
  officialAuthLabel: string;
  officialSaveLabel: string;
  cancelLabel: string;
  formEyebrow: string;
  formAddTitle: string;
  formEditTitle: string;
  formHint: string;
  apiConfigTitle: string;
  apiConfigDescription: string;
  apiKeyLabel: string;
  apiKeyPlaceholder: string;
  showApiKeyLabel: string;
  hideApiKeyLabel: string;
  baseUrlLabel: string;
  nameLabel: string;
  modelLabel: string;
  fetchModelsLabel: string;
  fetchingModelsLabel: string;
  chooseModelLabel: (count: number) => string;
  wireApiLabel: string;
  requiresAuthLabel: string;
  authPreviewTitle: string;
  authPreviewDescription: string;
  tomlTitle: string;
  tomlDescription: string;
  resetTomlLabel: string;
  saveLabel: string;
  savingLabel: string;
};

export type ProviderOfficialInfo = {
  officialUrl: ReactNode;
  authPath: ReactNode;
  current: ReactNode;
};

export type ProvidersPageProps = {
  lang: "zh" | "en";
  copy: ProviderCopy;
  mode: ProviderMode;
  providerRows: readonly ProviderRow[];
  loading: boolean;
  testingId: string;
  actionBusy?: string;
  editingProviderId: string | null;
  providerForm: ProviderFormValue;
  officialForm: OfficialFormValue;
  officialInfo: ProviderOfficialInfo;
  providerAuthPreview: ReactNode;
  providerTomlDraft: string;
  providerTomlRef?: Ref<HTMLTextAreaElement>;
  apiKeyVisible: boolean;
  availableModels: readonly string[];
  fetchingModels: boolean;
  onImportCcSwitch: () => void;
  onAddProvider: () => void;
  onEnableProvider: (row: ProviderRow) => void;
  onTestProvider: (row: ProviderRow) => void;
  onEditProvider: (row: ProviderRow) => void;
  onDeleteProvider: (row: ProviderRow) => Promise<boolean>;
  onCancelMode: () => void;
  onOfficialModelChange: (value: string) => void;
  onOfficialAuthChange: (value: string) => void;
  onSaveOfficial: () => void;
  onApiKeyChange: (value: string) => void;
  onBaseUrlChange: (value: string) => void;
  onProviderNameChange: (value: string) => void;
  onProviderModelChange: (value: string) => void;
  onFetchModels: () => void;
  onWireApiChange: (value: string) => void;
  onRequiresAuthChange: (value: boolean) => void;
  onToggleApiKeyVisibility: () => void;
  onProviderTomlDraftChange: (value: string) => void;
  onResetProviderToml: () => void;
  onSaveProvider: () => void;
};

type FieldProps = {
  label: string;
  children: ReactNode;
  className?: string;
};

function Field({ label, children, className }: FieldProps) {
  return (
    <label className={`cx-providers-field${className ? ` ${className}` : ""}`}>
      <span>{label}</span>
      {children}
    </label>
  );
}

function ProviderAvatar({ row }: { row: ProviderRow }) {
  const initial = row.providerName.trim().slice(0, 1).toUpperCase() || "?";
  const className = [
    "cx-providers-avatar",
    row.source === "official" ? "cx-providers-avatar--official" : "",
    row.isCurrent ? "cx-providers-avatar--current" : "",
  ].filter(Boolean).join(" ");

  return (
    <div className={className} aria-hidden="true">
      {row.source === "official" ? <span className="cx-providers-openai-logo" /> : initial}
    </div>
  );
}

function ActionIconButton({
  icon: Icon,
  label,
  onClick,
  disabled,
  danger = false,
}: {
  icon: LucideIcon;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      className={`cx-providers-icon-button${danger ? " cx-providers-icon-button--danger" : ""}`}
      title={label}
      aria-label={label}
      onClick={onClick}
      disabled={disabled}
    >
      <Icon size={15} strokeWidth={1.9} aria-hidden="true" />
    </button>
  );
}

function ListPage({
  copy,
  providerRows,
  loading,
  testingId,
  actionBusy,
  onImportCcSwitch,
  onAddProvider,
  onEnableProvider,
  onTestProvider,
  onEditProvider,
  onDeleteProvider,
}: Pick<ProvidersPageProps, "copy" | "providerRows" | "loading" | "testingId" | "actionBusy" | "onImportCcSwitch" | "onAddProvider" | "onEnableProvider" | "onTestProvider" | "onEditProvider" | "onDeleteProvider">) {
  const [providerToDelete, setProviderToDelete] = useState<ProviderRow | null>(null);
  const [deleting, setDeleting] = useState(false);

  const closeDeleteDialog = () => {
    if (!deleting) setProviderToDelete(null);
  };

  const confirmDelete = async () => {
    if (!providerToDelete || deleting) return;
    setDeleting(true);
    try {
      if (await onDeleteProvider(providerToDelete)) setProviderToDelete(null);
    } finally {
      setDeleting(false);
    }
  };

  return (
    <>
      <header className="cx-providers-header">
        <div className="cx-providers-header-copy">
          <div className="cx-providers-eyebrow">{copy.eyebrow}</div>
          <h2>{copy.title}</h2>
          <p>{copy.subtitle}</p>
        </div>
        <div className="cx-providers-header-actions">
          <button
            type="button"
            className="cx-providers-button cx-providers-button--secondary"
            onClick={onImportCcSwitch}
            disabled={loading || Boolean(actionBusy)}
          >
            {actionBusy === "importCcSwitch" ? <Loader2 size={15} className="cx-providers-spin" aria-hidden="true" /> : <RefreshCw size={15} aria-hidden="true" />}
            {copy.importLabel}
          </button>
          <button type="button" className="cx-providers-button cx-providers-button--dark" onClick={onAddProvider} disabled={loading}>
            <Plus size={15} aria-hidden="true" />
            {copy.addLabel}
          </button>
        </div>
      </header>

      <div className="cx-providers-list" role="list">
        {providerRows.length === 0 ? (
          <div className="cx-providers-empty" role="status">{copy.noProviders}</div>
        ) : providerRows.map((row) => {
          const testingKey = row.testingKey || `${row.source}-${row.id}`;
          const isTesting = testingId === testingKey;
          return (
            <article className={`cx-providers-row${row.isCurrent ? " cx-providers-row--current" : ""}`} key={`${row.source}-${row.id}-${row.baseUrl}`} role="listitem">
              <ProviderAvatar row={row} />
              <div className="cx-providers-row-main">
                <div className="cx-providers-row-title">
                  <strong>{row.providerName}</strong>
                  {row.sourceLabel && (
                    <span className={`cx-providers-source-badge${row.source === "official" ? " cx-providers-source-badge--official" : ""}`}>
                      {row.sourceLabel}
                    </span>
                  )}
                </div>
                <code title={row.baseUrl || copy.noBaseUrlLabel}>{row.baseUrl || copy.noBaseUrlLabel}</code>
                {row.meta && <div className="cx-providers-row-meta">{row.meta}</div>}
              </div>
              <div className="cx-providers-row-actions">
                {row.isCurrent && <span className="cx-providers-current-badge"><span aria-hidden="true" />{copy.currentLabel}</span>}
                <button
                  type="button"
                  className="cx-providers-button cx-providers-button--small cx-providers-button--secondary"
                  onClick={() => onEnableProvider(row)}
                  disabled={loading || row.isCurrent}
                >
                  {copy.enableLabel}
                </button>
                {row.testable !== false && (
                  <ActionIconButton
                    icon={isTesting ? Loader2 : Activity}
                    label={copy.testLabel}
                    onClick={() => onTestProvider(row)}
                    disabled={loading || isTesting}
                  />
                )}
                {row.editable !== false && (
                  <ActionIconButton icon={PencilLine} label={copy.editLabel} onClick={() => onEditProvider(row)} disabled={loading} />
                )}
                {row.deletable && (
                  <ActionIconButton icon={Trash2} label={copy.removeLabel} onClick={() => setProviderToDelete(row)} disabled={loading} danger />
                )}
              </div>
            </article>
          );
        })}
      </div>

      <ModalShell
        open={Boolean(providerToDelete)}
        onClose={closeDeleteDialog}
        title={copy.deleteTitle}
        description={providerToDelete
          ? providerToDelete.isCurrent
            ? copy.deleteCurrentDescription(providerToDelete.providerName)
            : copy.deleteDescription(providerToDelete.providerName)
          : undefined}
        size="sm"
        closeLabel={copy.deleteCancelLabel}
        closeOnBackdrop={!deleting}
        closeOnEscape={!deleting}
        showCloseButton={!deleting}
        className="cx-provider-delete-dialog"
        bodyClassName="cx-provider-delete-dialog-body"
        footer={(
          <>
            <Button variant="secondary" onClick={closeDeleteDialog} disabled={deleting} data-initial-focus>
              {copy.deleteCancelLabel}
            </Button>
            <Button variant="danger" icon={deleting ? <Loader2 size={16} className="cx-providers-spin" /> : <Trash2 size={16} />} onClick={() => void confirmDelete()} disabled={deleting}>
              {copy.deleteConfirmLabel}
            </Button>
          </>
        )}
      >
        <div className="cx-provider-delete-warning">
          <span aria-hidden="true"><AlertTriangle size={22} /></span>
          <strong>{providerToDelete?.providerName}</strong>
        </div>
      </ModalShell>
    </>
  );
}

function ModeHeader({ eyebrow, title, description, cancelLabel, onCancel }: { eyebrow: string; title: string; description: string; cancelLabel: string; onCancel: () => void }) {
  return (
    <header className="cx-providers-form-header">
      <div>
        <div className="cx-providers-eyebrow">{eyebrow}</div>
        <h2>{title}</h2>
        <p>{description}</p>
      </div>
      <button type="button" className="cx-providers-button cx-providers-button--secondary" onClick={onCancel}>
        <ArrowLeft size={15} aria-hidden="true" />
        {cancelLabel}
      </button>
    </header>
  );
}

function OfficialForm({
  copy,
  officialForm,
  officialInfo,
  loading,
  onCancelMode,
  onOfficialModelChange,
  onOfficialAuthChange,
  onSaveOfficial,
}: Pick<ProvidersPageProps, "copy" | "officialForm" | "officialInfo" | "loading" | "onCancelMode" | "onOfficialModelChange" | "onOfficialAuthChange" | "onSaveOfficial">) {
  return (
    <>
      <ModeHeader eyebrow={copy.officialEyebrow} title={copy.officialTitle} description={copy.officialHint} cancelLabel={copy.cancelLabel} onCancel={onCancelMode} />
      <div className="cx-providers-info-grid">
        <div><span>{copy.officialUrlLabel}</span><code>{officialInfo.officialUrl}</code></div>
        <div><span>{copy.authPathLabel}</span><code>{officialInfo.authPath}</code></div>
        <div><span>{copy.officialCurrentLabel}</span><code>{officialInfo.current}</code></div>
      </div>
      <div className="cx-providers-form-grid cx-providers-form-grid--single">
        <Field label={copy.modelLabel}><input value={officialForm.model} onChange={(event) => onOfficialModelChange(event.target.value)} /></Field>
      </div>
      <Field label={copy.officialAuthLabel} className="cx-providers-editor-field">
        <textarea
          className="cx-providers-code-editor cx-providers-auth-editor"
          value={officialForm.authJson}
          onChange={(event) => onOfficialAuthChange(event.target.value)}
          wrap="off"
          spellCheck={false}
        />
      </Field>
      <div className="cx-providers-form-actions cx-providers-form-actions--save">
        <button type="button" className="cx-providers-button cx-providers-button--primary" onClick={onSaveOfficial} disabled={loading}><CheckCircle2 size={15} aria-hidden="true" />{copy.officialSaveLabel}</button>
      </div>
    </>
  );
}

function ProviderForm({
  copy,
  providerForm,
  loading,
  editingProviderId,
  providerAuthPreview,
  providerTomlDraft,
  providerTomlRef,
  apiKeyVisible,
  availableModels,
  fetchingModels,
  onCancelMode,
  onApiKeyChange,
  onBaseUrlChange,
  onProviderNameChange,
  onProviderModelChange,
  onFetchModels,
  onWireApiChange,
  onRequiresAuthChange,
  onToggleApiKeyVisibility,
  onProviderTomlDraftChange,
  onResetProviderToml,
  onSaveProvider,
}: Pick<ProvidersPageProps, "copy" | "providerForm" | "loading" | "editingProviderId" | "providerAuthPreview" | "providerTomlDraft" | "providerTomlRef" | "apiKeyVisible" | "availableModels" | "fetchingModels" | "onCancelMode" | "onApiKeyChange" | "onBaseUrlChange" | "onProviderNameChange" | "onProviderModelChange" | "onFetchModels" | "onWireApiChange" | "onRequiresAuthChange" | "onToggleApiKeyVisibility" | "onProviderTomlDraftChange" | "onResetProviderToml" | "onSaveProvider">) {
  const modelListId = useId();
  const canFetchModels = Boolean(providerForm.baseUrl.trim() && providerForm.apiKey.trim());

  return (
    <>
      <ModeHeader
        eyebrow={copy.formEyebrow}
        title={editingProviderId ? copy.formEditTitle : copy.formAddTitle}
        description={copy.formHint}
        cancelLabel={copy.cancelLabel}
        onCancel={onCancelMode}
      />

      <section className="cx-providers-form-section">
        <div className="cx-providers-section-heading">
          <div><h3>{copy.apiConfigTitle}</h3><p>{copy.apiConfigDescription}</p></div>
        </div>
        <div className="cx-providers-form-grid cx-providers-form-grid--provider">
          <Field label={copy.apiKeyLabel} className="cx-providers-field--full">
            <div className="cx-providers-secret-input">
              <input
                type={apiKeyVisible ? "text" : "password"}
                value={providerForm.apiKey}
                onChange={(event) => onApiKeyChange(event.target.value)}
                placeholder={copy.apiKeyPlaceholder}
              />
              <button type="button" onClick={onToggleApiKeyVisibility} title={apiKeyVisible ? copy.hideApiKeyLabel : copy.showApiKeyLabel} aria-label={apiKeyVisible ? copy.hideApiKeyLabel : copy.showApiKeyLabel}>
                {apiKeyVisible ? <EyeOff size={15} aria-hidden="true" /> : <Eye size={15} aria-hidden="true" />}
              </button>
            </div>
          </Field>
          <Field label={copy.baseUrlLabel} className="cx-providers-field--full"><input value={providerForm.baseUrl} onChange={(event) => onBaseUrlChange(event.target.value)} /></Field>
          <Field label={copy.nameLabel}><input value={providerForm.providerName} onChange={(event) => onProviderNameChange(event.target.value)} /></Field>
          <Field label={copy.modelLabel}>
            <div className="cx-providers-model-input-row">
              <input
                value={providerForm.model}
                list={availableModels.length ? modelListId : undefined}
                aria-label={copy.modelLabel}
                onChange={(event) => onProviderModelChange(event.target.value)}
              />
              <button
                type="button"
                className="cx-providers-button cx-providers-button--secondary cx-providers-button--small cx-providers-fetch-models"
                onClick={onFetchModels}
                disabled={loading || fetchingModels || !canFetchModels}
                title={copy.fetchModelsLabel}
                aria-label={copy.fetchModelsLabel}
              >
                {fetchingModels
                  ? <Loader2 size={14} className="cx-providers-spin" aria-hidden="true" />
                  : <RefreshCw size={14} aria-hidden="true" />}
                {fetchingModels ? copy.fetchingModelsLabel : copy.fetchModelsLabel}
              </button>
            </div>
            {availableModels.length > 0 && (
              <>
                <datalist id={modelListId}>
                  {availableModels.map((model) => <option value={model} key={model} />)}
                </datalist>
                <select
                  className="cx-providers-model-select"
                  value=""
                  aria-label={copy.chooseModelLabel(availableModels.length)}
                  onChange={(event) => {
                    if (event.target.value) onProviderModelChange(event.target.value);
                  }}
                >
                  <option value="">{copy.chooseModelLabel(availableModels.length)}</option>
                  {availableModels.map((model) => <option value={model} key={model}>{model}</option>)}
                </select>
              </>
            )}
          </Field>
          <Field label={copy.wireApiLabel}>
            <select value={providerForm.wireApi} onChange={(event) => onWireApiChange(event.target.value)}>
              <option value="responses">responses</option>
              <option value="chat">chat</option>
            </select>
          </Field>
          <Checkbox
            className="cx-providers-checkbox cx-providers-checkbox--full"
            checked={providerForm.requiresOpenaiAuth}
            onCheckedChange={onRequiresAuthChange}
            label={copy.requiresAuthLabel}
          />
        </div>
      </section>

      <section className="cx-providers-form-section">
        <div className="cx-providers-section-heading"><div><h3>{copy.authPreviewTitle}</h3><p>{copy.authPreviewDescription}</p></div></div>
        <div className="cx-providers-preview">{providerAuthPreview}</div>
      </section>

      <section className="cx-providers-form-section">
        <div className="cx-providers-section-heading cx-providers-section-heading--with-action">
          <div><h3>{copy.tomlTitle}</h3><p>{copy.tomlDescription}</p></div>
          <button type="button" className="cx-providers-button cx-providers-button--secondary cx-providers-button--small" onClick={onResetProviderToml}><RefreshCw size={14} aria-hidden="true" />{copy.resetTomlLabel}</button>
        </div>
        <textarea ref={providerTomlRef} className="cx-providers-code-editor cx-providers-toml-editor" value={providerTomlDraft} onChange={(event) => onProviderTomlDraftChange(event.target.value)} spellCheck={false} />
      </section>

      <div className="cx-providers-form-actions cx-providers-form-actions--save">
        <button type="button" className="cx-providers-button cx-providers-button--primary" onClick={onSaveProvider} disabled={loading}>
          {loading ? <Loader2 size={15} className="cx-providers-spin" aria-hidden="true" /> : <CheckCircle2 size={15} aria-hidden="true" />}
          {loading ? copy.savingLabel : copy.saveLabel}
        </button>
      </div>
    </>
  );
}

export function ProvidersPage(props: ProvidersPageProps) {
  return (
    <PageTransition pageKey={`providers:${props.mode}`}>
      <section className={`cx-providers cx-page cx-providers--${props.mode}`}>
        {props.mode === "list" && <ListPage {...props} />}
        {props.mode === "official" && <OfficialForm {...props} />}
        {props.mode === "form" && <ProviderForm {...props} />}
      </section>
    </PageTransition>
  );
}
