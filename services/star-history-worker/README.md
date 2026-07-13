# Codex Star History Worker

Cloudflare Worker that serves near-real-time Star History SVGs for:

- `yynxxxxx/Codex-X`
- `yynxxxxx/Codex-5.5-codex-instruct-5.5`

The Worker stores only aggregated timestamps and counts in KV. Repository-scoped GitHub Actions collect the authenticated history inside GitHub every 15 minutes and send a minimized snapshot to the Worker; GitHub `star` webhooks provide faster updates between scheduled reconciliations. The short-lived `GITHUB_TOKEN` never leaves the Actions runner.

## Endpoints

```text
GET  /v1/charts/codex-x.svg?theme=light
GET  /v1/charts/codex-x.svg?theme=dark
GET  /v1/charts/codex-5-5.svg?theme=light
GET  /v1/charts/codex-5-5.svg?theme=dark
GET  /v1/data/codex-x
GET  /v1/data/codex-5-5
GET  /healthz
POST /v1/refresh
POST /v1/github/webhook
```

## Secrets

Set secrets through Wrangler; never commit them:

```bash
npx wrangler secret put INGEST_TOKEN
npx wrangler secret put WEBHOOK_SECRET
```

`INGEST_TOKEN` must also be stored as the `STAR_HISTORY_INGEST_TOKEN` Actions secret in both repositories. `WEBHOOK_SECRET` is configured on both GitHub repository webhooks.

## Verify And Deploy

```bash
npm test
npx wrangler deploy --dry-run
npx wrangler deploy
```
