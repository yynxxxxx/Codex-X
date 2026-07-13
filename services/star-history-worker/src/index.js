import {
  buildBaseline,
  mergeObservation,
  renderStarHistorySvg,
  renderUnavailableSvg,
} from "./history.js";

const RENDER_VERSION = "2";
const MAX_WEBHOOK_BYTES = 1024 * 1024;
const encoder = new TextEncoder();

function jsonResponse(value, status = 200, headers = {}) {
  return new Response(JSON.stringify(value, null, 2), {
    status,
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
      ...headers,
    },
  });
}

function parseRepositoryConfig(env) {
  const repositories = new Map();
  for (const repository of String(env.ALLOWED_REPOSITORIES || "").split(",")) {
    const canonical = repository.trim();
    if (canonical) repositories.set(canonical.toLowerCase(), canonical);
  }
  const aliases = new Map();
  for (const item of String(env.REPOSITORY_ALIASES || "").split(",")) {
    const separator = item.indexOf("=");
    if (separator <= 0) continue;
    const alias = item.slice(0, separator).trim().toLowerCase();
    const requested = item.slice(separator + 1).trim().toLowerCase();
    const repository = repositories.get(requested);
    if (alias && repository) aliases.set(alias, repository);
  }
  return { repositories, aliases };
}

function allowedRepository(env, requested) {
  const { repositories } = parseRepositoryConfig(env);
  return repositories.get(String(requested || "").trim().toLowerCase()) || null;
}

function datasetKey(repository) {
  return `repository:${repository.toLowerCase()}`;
}

function liveEventPrefix(repository) {
  return `live-event:${repository.toLowerCase()}:`;
}

async function loadDatasetWithLiveEvents(env, repository) {
  const dataset = await env.STAR_HISTORY.get(datasetKey(repository), "json");
  if (!dataset?.baseline?.length) return dataset;
  const listing = await env.STAR_HISTORY.list({ prefix: liveEventPrefix(repository), limit: 1000 });
  const baselineCheckedAt = Date.parse(dataset.checkedAt || "") || 0;
  let latest = null;
  for (const key of listing.keys) {
    const metadata = key.metadata;
    const observedAt = Date.parse(metadata?.observedAt || "");
    const currentStars = Number(metadata?.currentStars);
    if (!Number.isFinite(observedAt) || !Number.isFinite(currentStars) || observedAt <= baselineCheckedAt) continue;
    if (!latest || observedAt > latest.observedAt) latest = { observedAt, currentStars };
  }
  return latest
    ? mergeObservation(dataset, { currentStars: latest.currentStars, checkedAt: latest.observedAt })
    : dataset;
}

async function digest(value) {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", encoder.encode(value)));
}

async function secureEqual(left, right) {
  const [leftDigest, rightDigest] = await Promise.all([digest(left), digest(right)]);
  let difference = left.length === right.length ? 0 : 1;
  for (let index = 0; index < leftDigest.length; index += 1) {
    difference |= leftDigest[index] ^ rightDigest[index];
  }
  return difference === 0;
}

async function authorizeRefresh(request, env) {
  const header = request.headers.get("Authorization") || "";
  const token = header.startsWith("Bearer ") ? header.slice(7).trim() : "";
  return Boolean(env.INGEST_TOKEN && token && (await secureEqual(token, env.INGEST_TOKEN)));
}

function bytesToHex(bytes) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function verifyWebhookSignature(rawBody, signatureHeader, secret) {
  if (!secret || !signatureHeader?.startsWith("sha256=")) return false;
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signature = new Uint8Array(await crypto.subtle.sign("HMAC", key, encoder.encode(rawBody)));
  return secureEqual(bytesToHex(signature), signatureHeader.slice(7).toLowerCase());
}

async function readTextWithLimit(request, maxBytes) {
  const declaredLength = Number(request.headers.get("Content-Length"));
  if (Number.isFinite(declaredLength) && declaredLength > maxBytes) throw new Error("Request body is too large");
  if (!request.body) return "";
  const reader = request.body.getReader();
  const chunks = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > maxBytes) {
      await reader.cancel();
      throw new Error("Request body is too large");
    }
    chunks.push(value);
  }
  const body = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(body);
}

async function ingestRepositorySnapshot(env, repository, snapshot, rebuild = false) {
  const createdAt = Date.parse(snapshot?.createdAt || "");
  const checkedAt = Date.parse(snapshot?.checkedAt || "");
  const currentStars = Number(snapshot?.currentStars);
  if (!Number.isFinite(createdAt) || !Number.isFinite(checkedAt)) {
    throw new Error("Snapshot timestamps are invalid");
  }
  if (!Number.isInteger(currentStars) || currentStars < 0) {
    throw new Error("Snapshot star count is invalid");
  }

  const existing = await env.STAR_HISTORY.get(datasetKey(repository), "json");
  let dataset;

  if (!existing?.baseline?.length || rebuild) {
    if (!Array.isArray(snapshot?.stargazers)) throw new Error("Snapshot stargazers are missing");
    dataset = buildBaseline({
      repository,
      createdAt,
      currentStars,
      stargazers: snapshot.stargazers,
      checkedAt,
    });
    const tolerance = Math.max(5, Math.ceil(dataset.currentStars * 0.01));
    if (Math.abs(dataset.source.consistencyDelta) > tolerance) {
      throw new Error(`GitHub stargazer snapshot mismatch: ${dataset.source.consistencyDelta}`);
    }
  } else {
    dataset = mergeObservation(existing, {
      currentStars,
      checkedAt,
    });
  }

  await env.STAR_HISTORY.put(datasetKey(repository), JSON.stringify(dataset));
  return dataset;
}

function parseChartRequest(url, env) {
  const match = url.pathname.match(/^\/v1\/charts\/([a-z0-9-]+)(?:\.(dark|light))?\.svg$/i);
  if (!match) return null;
  const { aliases } = parseRepositoryConfig(env);
  const repository = aliases.get(match[1].toLowerCase());
  if (!repository) return null;
  const queryTheme = url.searchParams.get("theme");
  const theme = match[2] || (queryTheme === "dark" ? "dark" : "light");
  return { repository, theme };
}

function chartHeaders(etag) {
  return {
    "Content-Type": "image/svg+xml; charset=utf-8",
    "Cache-Control": "no-cache, stale-if-error=86400",
    ETag: etag,
    "Access-Control-Allow-Origin": "*",
    "X-Content-Type-Options": "nosniff",
    "X-Robots-Tag": "noindex",
  };
}

async function serveChart(request, env, chart) {
  const dataset = await loadDatasetWithLiveEvents(env, chart.repository);
  const svg = dataset
    ? renderStarHistorySvg(dataset, chart.theme)
    : renderUnavailableSvg(chart.repository, chart.theme);
  const version = dataset?.checkedAt || "pending";
  const etag = `W/"v${RENDER_VERSION}-${chart.repository.toLowerCase().replaceAll(/[^a-z0-9]+/g, "-")}-${chart.theme}-${version}-${dataset?.currentStars ?? 0}"`;
  const headers = chartHeaders(etag);
  const validators = String(request.headers.get("If-None-Match") || "").split(",").map((value) => value.trim());
  if (validators.includes("*") || validators.includes(etag)) return new Response(null, { status: 304, headers });
  return new Response(request.method === "HEAD" ? null : svg, { status: 200, headers });
}

async function handleRefresh(request, env) {
  if (!(await authorizeRefresh(request, env))) return jsonResponse({ error: "Unauthorized" }, 401);
  const payload = await request.json().catch(() => null);
  const repository = allowedRepository(env, payload?.repository);
  if (!repository) return jsonResponse({ error: "Repository is not allowed" }, 403);

  try {
    const dataset = await ingestRepositorySnapshot(env, repository, payload, payload?.rebuild === true);
    return jsonResponse({
      ok: true,
      repository,
      stars: dataset.currentStars,
      checkedAt: dataset.checkedAt,
      initializedAt: dataset.initializedAt,
      consistencyDelta: dataset.source?.consistencyDelta ?? 0,
    });
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    console.error("Star history refresh failed", repository, detail);
    return jsonResponse({
      error: "Snapshot refresh failed; previous chart data was preserved",
      detail,
    }, 502);
  }
}

async function handleWebhook(request, env) {
  let rawBody;
  try {
    rawBody = await readTextWithLimit(request, MAX_WEBHOOK_BYTES);
  } catch {
    return jsonResponse({ error: "Webhook body is too large" }, 413);
  }
  const valid = await verifyWebhookSignature(
    rawBody,
    request.headers.get("X-Hub-Signature-256"),
    env.WEBHOOK_SECRET,
  );
  if (!valid) return jsonResponse({ error: "Invalid webhook signature" }, 401);
  if (request.headers.get("X-GitHub-Event") !== "star") return jsonResponse({ ignored: true }, 202);

  let payload;
  try {
    payload = JSON.parse(rawBody);
  } catch {
    return jsonResponse({ error: "Webhook JSON is invalid" }, 400);
  }
  const repository = allowedRepository(env, payload?.repository?.full_name);
  if (!repository) return jsonResponse({ error: "Repository is not allowed" }, 403);
  const currentStars = payload?.repository?.stargazers_count;
  if (!Number.isFinite(currentStars)) return jsonResponse({ error: "Webhook star count is missing" }, 400);
  const delivery = request.headers.get("X-GitHub-Delivery")?.trim();
  if (!delivery || !/^[a-z0-9-]{8,80}$/i.test(delivery)) return jsonResponse({ error: "Webhook delivery ID is missing" }, 400);
  const eventKey = `${liveEventPrefix(repository)}${delivery.toLowerCase()}`;
  if (await env.STAR_HISTORY.get(eventKey)) return jsonResponse({ duplicate: true }, 202);
  const observedAt = new Date().toISOString();
  await env.STAR_HISTORY.put(eventKey, "1", {
    expirationTtl: 24 * 60 * 60,
    metadata: {
      action: payload.action || "unknown",
      currentStars,
      eventAt: payload.starred_at || null,
      observedAt,
    },
  });
  return jsonResponse({ accepted: true, repository, stars: currentStars }, 202);
}

async function handleHealth(env) {
  const { repositories } = parseRepositoryConfig(env);
  const rows = await Promise.all([...repositories.values()].map(async (repository) => {
    const dataset = await loadDatasetWithLiveEvents(env, repository);
    const checkedAt = dataset?.checkedAt || null;
    return {
      repository,
      ready: Boolean(dataset?.baseline?.length),
      stars: dataset?.currentStars ?? null,
      checkedAt,
      stale: checkedAt ? Date.now() - Date.parse(checkedAt) > 30 * 60 * 1000 : true,
    };
  }));
  const healthy = rows.every((row) => row.ready && !row.stale);
  return jsonResponse({ ok: healthy, repositories: rows }, healthy ? 200 : 503, {
    "Cache-Control": "no-cache",
  });
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (request.method === "OPTIONS") {
      return new Response(null, {
        status: 204,
        headers: {
          "Access-Control-Allow-Origin": "*",
          "Access-Control-Allow-Methods": "GET, HEAD, POST, OPTIONS",
          "Access-Control-Allow-Headers": "Authorization, Content-Type, X-Hub-Signature-256, X-GitHub-Event, X-GitHub-Delivery",
        },
      });
    }

    if (request.method === "POST" && url.pathname === "/v1/refresh") return handleRefresh(request, env);
    if (request.method === "POST" && url.pathname === "/v1/github/webhook") return handleWebhook(request, env);
    if (request.method === "GET" && url.pathname === "/healthz") return handleHealth(env);

    const chart = parseChartRequest(url, env);
    if ((request.method === "GET" || request.method === "HEAD") && chart) return serveChart(request, env, chart);

    if (request.method === "GET" && url.pathname.startsWith("/v1/data/")) {
      const alias = url.pathname.slice("/v1/data/".length).toLowerCase();
      const { aliases } = parseRepositoryConfig(env);
      const repository = aliases.get(alias);
      if (!repository) return jsonResponse({ error: "Not found" }, 404);
      const dataset = await loadDatasetWithLiveEvents(env, repository);
      return dataset ? jsonResponse(dataset, 200, { "Cache-Control": "no-cache" }) : jsonResponse({ error: "Not initialized" }, 404);
    }

    if (request.method === "GET" && url.pathname === "/") {
      return jsonResponse({
        service: "Codex Star History",
        charts: {
          "codex-x": "/v1/charts/codex-x.svg",
          "codex-5-5": "/v1/charts/codex-5-5.svg",
        },
        refresh: "GitHub Actions every 15 minutes plus GitHub star webhooks",
      }, 200, { "Cache-Control": "public, max-age=300" });
    }

    return jsonResponse({ error: "Not found" }, 404);
  },
};
