import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";
import ts from "typescript";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const desktop = dirname(dirname(fileURLToPath(import.meta.url)));
let page;
async function sessionPage() {
  if (page) return page;
  page = (async () => {
    const scratch = await mkdtemp(join(desktop, "tests/.session-presentation-"));
    try {
      const emitted = new Map();
      async function compile(sourcePath) {
        if (emitted.has(sourcePath)) return emitted.get(sourcePath);
        const outputPath = join(scratch, relative(desktop, sourcePath).replace(/\.(?:tsx?|json)$/, ".mjs"));
        emitted.set(sourcePath, outputPath);
        const source = await readFile(sourcePath, "utf8");
        let output = sourcePath.endsWith(".json")
          ? `export default JSON.parse(${JSON.stringify(source)});`
          : ts.transpileModule(source, { fileName: sourcePath, compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.ESNext, jsx: ts.JsxEmit.ReactJSX } }).outputText.replace(/import\s*["'][^"']+\.css["'];?/g, "");
        for (const match of [...output.matchAll(/\bfrom\s*["'](\.[^"']+)["']/g)]) {
          const base = resolve(dirname(sourcePath), match[1]);
          let dependency;
          for (const candidate of [base, `${base}.ts`, `${base}.tsx`, join(base, "index.ts")]) {
            try { await readFile(candidate, "utf8"); dependency = candidate; break; } catch { /* Try the next module extension. */ }
          }
          assert.ok(dependency, `could not resolve ${match[1]}`);
          let replacement = relative(dirname(outputPath), await compile(dependency)).replaceAll("\\", "/");
          if (!replacement.startsWith(".")) replacement = `./${replacement}`;
          output = output.replaceAll(`"${match[1]}"`, `"${replacement}"`).replaceAll(`'${match[1]}'`, `'${replacement}'`);
        }
        await mkdir(dirname(outputPath), { recursive: true });
        await writeFile(outputPath, output);
        return outputPath;
      }
      return (await import(pathToFileURL(await compile(join(desktop, "src/pages/SessionManagementPage.tsx"))).href)).SessionManagementPage;
    } finally {
      await rm(scratch, { recursive: true, force: true });
    }
  })();
  return page;
}

const session = { id: "fixture", title: "Renamed session", firstUserMessage: "Initial <prompt>", cwd: "/fixture/project", archived: false, hasUserEvent: true, isSubagent: false, needsSync: true };
function props(readOnlyReason, extra = {}) {
  return {
    active: true, lang: "en", sessionStatus: { codexDir: "/fixture/.codex", topLevelThreads: 1, subagentThreads: 0, scanComplete: true, scanFailures: [], warnings: readOnlyReason ? [readOnlyReason] : [], readOnlyReason, sessions: [session] },
    sessionHasMismatches: true, sessionSyncCount: 1, sessionTargetLabel: "Fixture", sessionVisibleTotal: 1, sessionPreviewTruncated: false,
    visibleSessions: [session], filteredSessions: [session], allSessionsByCwd: new Map([[session.cwd, [session]]]), groupedSessions: [[session.cwd, [session]]],
    selectedSessionIds: [session.id], selectedSessionSet: new Set([session.id]), selectedSessions: [session], sessionQuery: "", sessionGroupByCwd: false, showInternalSessions: false,
    loading: false, actionBusy: "", sessionDeleteConfirmOpen: false, sessionDeleteBusy: false, sessionExportBusy: false, sessionDeleteSafetyConfirmed: true,
    ...extra,
  };
}
const buttons = html => [...html.matchAll(/<button\b[^>]*>[\s\S]*?<\/button>/g)].map(([button]) => button);
const button = (html, label) => {
  const found = buttons(html).find(button => button.includes(label));
  assert.ok(found, `button ${label}`);
  return found;
};

test("session rows show the name while the initial prompt is the escaped hover text", async () => {
  const html = renderToStaticMarkup(createElement(await sessionPage(), props()));
  assert.match(html, /<strong title="Initial &lt;prompt&gt;">Renamed session<\/strong>/);
  assert.doesNotMatch(button(html, "Sync sessions"), /\sdisabled=/);
  assert.doesNotMatch(button(html, "Delete 1 permanently"), /\sdisabled=/);
});

test("WSL read-only preview remains browsable and exportable while sync and deletion stay disabled", async () => {
  const html = renderToStaticMarkup(createElement(await sessionPage(), props("Native WSL preview: sync and deletion require Linux locks.", { sessionDeleteConfirmOpen: true })));
  assert.match(html, /WSL sessions · read-only/);
  assert.doesNotMatch(html, /All sessions are synced/);
  assert.match(html, /Renamed session/);
  assert.match(button(html, "Sync sessions"), /\sdisabled=/);
  assert.match(button(html, "Delete 1 permanently"), /\sdisabled=/);
  assert.doesNotMatch(button(html, "Export selected"), /\sdisabled=/);
  assert.doesNotMatch(html, /role="dialog"/);
});
