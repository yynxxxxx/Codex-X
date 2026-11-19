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
  assert.match(html, /role="row" title="First prompt: Initial &lt;prompt&gt;"/);
  assert.match(html, /<strong>Renamed session<\/strong>/);
  const describedBy = html.match(/aria-describedby="([^"]+)"/);
  assert.ok(describedBy, "the keyboard-focusable selection control describes the first message");
  assert.ok(html.includes(`<span id="${describedBy[1]}" hidden="">First prompt: Initial &lt;prompt&gt;</span>`));
  assert.doesNotMatch(button(html, "Sync sessions"), /\sdisabled=/);
  assert.doesNotMatch(button(html, "Delete 1 permanently"), /\sdisabled=/);
});

test("WSL read-only preview remains browsable and exportable while sync and deletion stay disabled", async () => {
  const html = renderToStaticMarkup(createElement(await sessionPage(), props("Native WSL preview: sync and deletion require Linux locks.", { sessionDeleteConfirmOpen: true })));
  assert.match(html, /WSL sessions · read-only/);
  assert.doesNotMatch(html, /All sessions are synced/);
  assert.match(html, /Renamed session/);
  assert.match(html, /role="row" title="First prompt: Initial &lt;prompt&gt;"/);
  assert.match(button(html, "Sync sessions"), /\sdisabled=/);
  assert.match(button(html, "Delete 1 permanently"), /\sdisabled=/);
  assert.doesNotMatch(button(html, "Export selected"), /\sdisabled=/);
  assert.doesNotMatch(html, /role="dialog"/);
});

test("long multiline prompts have bounded escaped previews while absent prompts are not invented", async () => {
  const Page = await sessionPage();
  const renderSession = item => renderToStaticMarkup(createElement(Page, props(undefined, {
    visibleSessions: [item], filteredSessions: [item], allSessionsByCwd: new Map([[item.cwd, [item]]]), groupedSessions: [[item.cwd, [item]]],
  })));
  const longPrompt = `First line <&>\n${"x".repeat(1984)}😀tail`;
  const html = renderSession({ ...session, firstUserMessage: longPrompt });
  assert.match(html, /role="row" title="First prompt: First line &lt;&amp;&gt;\n/);
  assert.match(html, /…"/);
  assert.doesNotMatch(html, /tail|\uFFFD|[\uD800-\uDBFF](?![\uDC00-\uDFFF])/u);
  assert.match(html, /<strong>Renamed session<\/strong>/);
  const missing = renderSession({ ...session, firstUserMessage: " \n " });
  assert.match(missing, /role="row" title="Renamed session"/);
  assert.doesNotMatch(missing, /First prompt:|aria-describedby=/);
});

test("syncing and checking hide snapshot warnings while keeping session previews visible", async () => {
  const Page = await sessionPage();
  for (const [actionBusy, label] of [["syncSessions", "Syncing..."], ["checkSessions", "Checking..."]]) {
    const html = renderToStaticMarkup(createElement(Page, props(undefined, {
      loading: true, actionBusy, sessionSyncCount: 13,
    })));
    assert.match(html, /class="cx-session-summary"[^>]*aria-busy="true"/);
    assert.ok(html.includes(label));
    assert.doesNotMatch(html, /13 session\(s\) need syncing|cx-session-summary--needs-sync|cx-session-summary--synced|cx-session-row--needs-sync|cx-session-state--warn/);
    assert.match(html, /<strong>Renamed session<\/strong>/);
    assert.match(html, /role="row" title="First prompt: Initial &lt;prompt&gt;"/);
    assert.match(button(html, label), /\sdisabled=/);
    const previouslySynced = renderToStaticMarkup(createElement(Page, props(undefined, {
      loading: true, actionBusy, sessionHasMismatches: false, sessionSyncCount: 0,
    })));
    assert.doesNotMatch(previouslySynced, /All sessions are synced|cx-session-summary--synced/);
  }
});

test("a fresh pending result restores its count and retry action after checking", async () => {
  const html = renderToStaticMarkup(createElement(await sessionPage(), props()));
  assert.match(html, /1 session\(s\) need syncing/);
  assert.match(html, /cx-session-summary--needs-sync/);
  assert.match(html, /cx-session-row--needs-sync/);
  assert.match(html, /cx-session-state--warn/);
  assert.doesNotMatch(button(html, "Check sessions"), /\sdisabled=/);
  assert.doesNotMatch(button(html, "Sync sessions"), /\sdisabled=/);
});

test("an unavailable recheck result stays neutral and allows another check", async () => {
  const html = renderToStaticMarkup(createElement(await sessionPage(), props(undefined, {
    sessionStatus: null, sessionHasMismatches: false, sessionSyncCount: 0,
    visibleSessions: [], filteredSessions: [], groupedSessions: [], allSessionsByCwd: new Map(),
  })));
  assert.match(html, /Check sessions to get started/);
  assert.doesNotMatch(html, /All sessions are synced|session\(s\) need syncing|cx-session-summary--synced|cx-session-summary--needs-sync|cx-session-state--warn/);
  assert.doesNotMatch(button(html, "Check sessions"), /\sdisabled=/);
  assert.match(button(html, "Sync sessions"), /\sdisabled=/);
});
