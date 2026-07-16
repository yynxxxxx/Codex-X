import type { ReactNode } from "react";
import {
  CheckCircle2,
  Download,
  ExternalLink,
  Globe2,
  Loader2,
  RefreshCw,
  Sparkles,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import "../styles/utility-pages.css";

export type UtilityLanguage = "zh" | "en";
export type UtilityStatusTone = "neutral" | "success" | "warning" | "error";

type PageHeaderProps = {
  eyebrow: ReactNode;
  title: ReactNode;
  description?: ReactNode;
  aside?: ReactNode;
};

function PageHeader({ eyebrow, title, description, aside }: PageHeaderProps) {
  return (
    <header className="cx-page-header">
      <div className="cx-page-header-copy">
        <div className="cx-page-eyebrow">{eyebrow}</div>
        <h2>{title}</h2>
        {description && <p>{description}</p>}
      </div>
      {aside}
    </header>
  );
}

export type TomlConfigPageProps = {
  eyebrow: ReactNode;
  title: ReactNode;
  description: ReactNode;
  loaded: ReactNode;
  isLoaded: boolean;
  preview: ReactNode;
};

export function TomlConfigPage({
  eyebrow,
  title,
  description,
  loaded,
  isLoaded,
  preview,
}: TomlConfigPageProps) {
  return (
    <section className="cx-utility cx-page cx-page--toml">
      <PageHeader
        eyebrow={eyebrow}
        title={title}
        description={description}
        aside={(
          <div className={`cx-page-header-status${isLoaded ? "" : " cx-page-header-status--missing"}`} aria-live="polite">
            <span className="cx-page-status-dot" aria-hidden="true" />
            <span>{loaded}</span>
          </div>
        )}
      />
      <section className="cx-page-panel cx-page-code-panel">
        <div className="cx-page-code-frame">{preview}</div>
      </section>
    </section>
  );
}

export type SettingsCopy = {
  eyebrow: ReactNode;
  title: ReactNode;
  languageTitle: ReactNode;
  languageDescription: ReactNode;
  chineseLabel: ReactNode;
  englishLabel: ReactNode;
  productTitle: ReactNode;
  productDescription: ReactNode;
  productValue: ReactNode;
  recheckTitle: ReactNode;
  recheckDescription: ReactNode;
  recheckLabel: ReactNode;
};

export type SettingsPageProps = {
  lang: UtilityLanguage;
  copy: SettingsCopy;
  onLanguageChange: (lang: UtilityLanguage) => void;
  onRecheck: () => void;
  recheckBusy?: boolean;
};

type SettingRowProps = {
  icon: LucideIcon;
  title: ReactNode;
  description: ReactNode;
  action: ReactNode;
};

function SettingRow({ icon: Icon, title, description, action }: SettingRowProps) {
  return (
    <div className="cx-page-setting-row">
      <div className="cx-page-setting-icon" aria-hidden="true">
        <Icon size={18} strokeWidth={1.9} />
      </div>
      <div className="cx-page-setting-copy">
        <strong>{title}</strong>
        <p>{description}</p>
      </div>
      <div className="cx-page-setting-action">{action}</div>
    </div>
  );
}

export function SettingsPage({
  lang,
  copy,
  onLanguageChange,
  onRecheck,
  recheckBusy = false,
}: SettingsPageProps) {
  return (
    <section className="cx-utility cx-page cx-page--settings">
      <PageHeader eyebrow={copy.eyebrow} title={copy.title} />
      <div className="cx-page-settings-list">
        <SettingRow
          icon={Globe2}
          title={copy.languageTitle}
          description={copy.languageDescription}
          action={(
            <div className="cx-page-segmented" role="group" aria-label={String(copy.languageTitle)}>
              <button
                type="button"
                className={lang === "zh" ? "cx-page-segmented-button cx-page-segmented-button--active" : "cx-page-segmented-button"}
                onClick={() => onLanguageChange("zh")}
                aria-pressed={lang === "zh"}
              >
                {copy.chineseLabel}
              </button>
              <button
                type="button"
                className={lang === "en" ? "cx-page-segmented-button cx-page-segmented-button--active" : "cx-page-segmented-button"}
                onClick={() => onLanguageChange("en")}
                aria-pressed={lang === "en"}
              >
                {copy.englishLabel}
              </button>
            </div>
          )}
        />

        <SettingRow
          icon={Sparkles}
          title={copy.productTitle}
          description={copy.productDescription}
          action={<span className="cx-page-value-pill">{copy.productValue}</span>}
        />

        <SettingRow
          icon={CheckCircle2}
          title={copy.recheckTitle}
          description={copy.recheckDescription}
          action={(
            <button
              type="button"
              className="cx-page-button cx-page-button--secondary"
              onClick={onRecheck}
              disabled={recheckBusy}
            >
              {recheckBusy && <Loader2 size={15} className="cx-page-spin" aria-hidden="true" />}
              {copy.recheckLabel}
            </button>
          )}
        />
      </div>
    </section>
  );
}

export type AboutCopy = {
  eyebrow: ReactNode;
  title: ReactNode;
  appVersionLabel: ReactNode;
  codexVersionLabel: ReactNode;
  codexHomeLabel: ReactNode;
  projectLabel: ReactNode;
  openProjectLabel: ReactNode;
  openIssuesLabel: ReactNode;
  releasesEyebrow: ReactNode;
  releasesTitle: ReactNode;
  releaseStatusLabel: ReactNode;
  latestVersionLabel: ReactNode;
  checkUpdateLabel: ReactNode;
  openReleasesLabel: ReactNode;
};

export type AboutReleaseState = {
  status: ReactNode;
  latestVersion: ReactNode;
  tone?: UtilityStatusTone;
  checking?: boolean;
  canOpenReleases?: boolean;
};

export type AboutPageProps = {
  copy: AboutCopy;
  appVersion: ReactNode;
  codexVersion: ReactNode;
  codexHome: ReactNode;
  projectUrl: ReactNode;
  release: AboutReleaseState;
  onOpenProject: () => void;
  onOpenIssues: () => void;
  onCheckUpdate: () => void;
  onOpenReleases: () => void;
};

type InfoRowProps = {
  label: ReactNode;
  value: ReactNode;
  mono?: boolean;
};

function InfoRow({ label, value, mono = false }: InfoRowProps) {
  return (
    <div className="cx-page-info-row">
      <span>{label}</span>
      <strong className={mono ? "cx-page-info-value cx-page-info-value--mono" : "cx-page-info-value"}>{value}</strong>
    </div>
  );
}

export function AboutPage({
  copy,
  appVersion,
  codexVersion,
  codexHome,
  projectUrl,
  release,
  onOpenProject,
  onOpenIssues,
  onCheckUpdate,
  onOpenReleases,
}: AboutPageProps) {
  const releaseTone = release.tone || "neutral";
  const releaseStatusClass = `cx-page-release-status cx-page-release-status--${releaseTone}`;

  return (
    <section className="cx-utility cx-page cx-page--about">
      <PageHeader eyebrow={copy.eyebrow} title={copy.title} />

      <section className="cx-page-panel cx-page-about-panel">
        <div className="cx-page-info-list">
          <InfoRow label={copy.appVersionLabel} value={appVersion} />
          <InfoRow label={copy.codexVersionLabel} value={codexVersion} />
          <InfoRow label={copy.codexHomeLabel} value={codexHome} mono />
          <InfoRow label={copy.projectLabel} value={projectUrl} mono />
        </div>
        <div className="cx-page-panel-actions">
          <button type="button" className="cx-page-button cx-page-button--secondary" onClick={onOpenProject}>
            <ExternalLink size={15} aria-hidden="true" />
            {copy.openProjectLabel}
          </button>
          <button type="button" className="cx-page-button cx-page-button--secondary" onClick={onOpenIssues}>
            <ExternalLink size={15} aria-hidden="true" />
            {copy.openIssuesLabel}
          </button>
        </div>
      </section>

      <section className="cx-page-panel cx-page-release-panel">
        <div className="cx-page-release-header">
          <div>
            <div className="cx-page-eyebrow cx-page-eyebrow--muted">{copy.releasesEyebrow}</div>
            <h3>{copy.releasesTitle}</h3>
          </div>
          <span className={releaseStatusClass} aria-live="polite">{release.status}</span>
        </div>
        <div className="cx-page-info-list">
          <InfoRow label={copy.releaseStatusLabel} value={release.status} />
          <InfoRow label={copy.latestVersionLabel} value={release.latestVersion} />
        </div>
        <div className="cx-page-panel-actions">
          <button
            type="button"
            className="cx-page-button cx-page-button--primary"
            onClick={onCheckUpdate}
            disabled={release.checking}
          >
            {release.checking ? <Loader2 size={15} className="cx-page-spin" aria-hidden="true" /> : <RefreshCw size={15} aria-hidden="true" />}
            {copy.checkUpdateLabel}
          </button>
          <button
            type="button"
            className="cx-page-button cx-page-button--secondary"
            onClick={onOpenReleases}
            disabled={release.canOpenReleases === false}
          >
            <Download size={15} aria-hidden="true" />
            {copy.openReleasesLabel}
          </button>
        </div>
      </section>
    </section>
  );
}
