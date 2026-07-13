import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import test from "node:test";

import worker from "../src/index.js";
import { buildBaseline } from "../src/history.js";

class MemoryKv {
  constructor() {
    this.values = new Map();
  }

  async get(key, type) {
    const item = this.values.get(key);
    if (!item) return null;
    return type === "json" ? JSON.parse(item.value) : item.value;
  }

  async put(key, value, options = {}) {
    this.values.set(key, { value, metadata: options.metadata ?? null });
  }

  async list({ prefix = "" } = {}) {
    return {
      keys: [...this.values.entries()]
        .filter(([key]) => key.startsWith(prefix))
        .map(([name, item]) => ({ name, metadata: item.metadata })),
      list_complete: true,
    };
  }
}

function testEnv(dataset) {
  const kv = new MemoryKv();
  if (dataset) kv.values.set("repository:yynxxxxx/codex-x", { value: JSON.stringify(dataset), metadata: null });
  return {
    STAR_HISTORY: kv,
    ALLOWED_REPOSITORIES: "yynxxxxx/Codex-X",
    REPOSITORY_ALIASES: "codex-x=yynxxxxx/Codex-X",
    INGEST_TOKEN: "test-ingest",
    WEBHOOK_SECRET: "test-webhook",
  };
}

function baseline(checkedAt = new Date().toISOString()) {
  return buildBaseline({
    repository: "yynxxxxx/Codex-X",
    createdAt: "2026-01-01T00:00:00Z",
    currentStars: 10,
    checkedAt,
    stargazers: [],
  });
}

test("chart responses support ETag revalidation with multiple validators", async () => {
  const env = testEnv(baseline());
  const first = await worker.fetch(new Request("https://example.test/v1/charts/codex-x.svg"), env);
  const etag = first.headers.get("ETag");
  assert.equal(first.status, 200);
  assert.ok(etag?.startsWith('W/"v2-'));
  assert.match(await first.text(), /^<svg/);

  const second = await worker.fetch(new Request("https://example.test/v1/charts/codex-x.svg", {
    headers: { "If-None-Match": `W/"other", ${etag}` },
  }), env);
  assert.equal(second.status, 304);
});

test("health returns 503 when the last authoritative refresh is stale", async () => {
  const staleAt = new Date(Date.now() - 31 * 60 * 1000).toISOString();
  const env = testEnv(baseline(staleAt));
  const response = await worker.fetch(new Request("https://example.test/healthz"), env);
  const payload = await response.json();
  assert.equal(response.status, 503);
  assert.equal(payload.ok, false);
  assert.equal(payload.repositories[0].stale, true);
});

test("refresh ingests an Actions snapshot and stores aggregated history", async () => {
  const env = testEnv();
  const payload = {
    repository: "yynxxxxx/Codex-X",
    createdAt: "2026-01-01T00:00:00Z",
    checkedAt: "2026-01-03T00:00:00Z",
    currentStars: 2,
    stargazers: [
      { user: { id: 1 }, starred_at: "2026-01-01T12:00:00Z" },
      { user: { id: 1 }, starred_at: "2026-01-02T12:00:00Z" },
      { user: { id: 2 }, starred_at: "2026-01-02T13:00:00Z" },
    ],
  };
  const response = await worker.fetch(new Request("https://example.test/v1/refresh", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${env.INGEST_TOKEN}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(payload),
  }), env);
  const result = await response.json();
  const stored = await env.STAR_HISTORY.get("repository:yynxxxxx/codex-x", "json");

  assert.equal(response.status, 200);
  assert.equal(result.stars, 2);
  assert.equal(stored.source.uniqueStargazers, 2);
  assert.equal(stored.baseline.at(-1).count, 2);
  assert.equal("stargazers" in stored, false);
});

test("webhook events use unique keys and duplicate deliveries stay idempotent", async () => {
  const env = testEnv(baseline("2026-01-02T00:00:00Z"));
  const payload = JSON.stringify({
    action: "created",
    starred_at: "2026-01-02T00:01:00Z",
    repository: { full_name: "yynxxxxx/Codex-X", stargazers_count: 11 },
  });
  const signature = createHmac("sha256", env.WEBHOOK_SECRET).update(payload).digest("hex");
  const headers = {
    "Content-Type": "application/json",
    "X-GitHub-Delivery": "33333333-3333-4333-8333-333333333333",
    "X-GitHub-Event": "star",
    "X-Hub-Signature-256": `sha256=${signature}`,
  };

  const first = await worker.fetch(new Request("https://example.test/v1/github/webhook", {
    method: "POST",
    headers,
    body: payload,
  }), env);
  const duplicate = await worker.fetch(new Request("https://example.test/v1/github/webhook", {
    method: "POST",
    headers,
    body: payload,
  }), env);
  const data = await worker.fetch(new Request("https://example.test/v1/data/codex-x"), env);
  const dataset = await data.json();

  assert.equal(first.status, 202);
  assert.equal(duplicate.status, 202);
  assert.equal((await duplicate.json()).duplicate, true);
  assert.equal(dataset.currentStars, 11);
});

test("webhook rejects oversized payloads before signature work", async () => {
  const env = testEnv(baseline());
  const response = await worker.fetch(new Request("https://example.test/v1/github/webhook", {
    method: "POST",
    headers: { "Content-Length": String(1024 * 1024 + 1) },
    body: "{}",
  }), env);
  assert.equal(response.status, 413);
});
