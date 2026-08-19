# Codex-X Architecture

## Purpose

Codex-X manages the active Codex configuration without owning Codex itself. The
application must preserve user-authored configuration where possible and must
never publish an OpenAI Official credential while a third-party endpoint is
active.

## Frontend

The desktop frontend is layered as follows:

```text
main.tsx
  -> app/App.tsx
  -> features/<domain>/api.ts
  -> Tauri IPC
```

`main.tsx` is bootstrap only. `app/App.tsx` owns the application shell,
application-wide state, tab coordination, startup dialog, toast/error display,
and feature mounting. It must not introduce Tauri command strings.

`app/appStorage.ts` is the only frontend owner of persisted browser settings:

- language and theme
- startup wizard completion
- selected third-party provider ID
- prompt injection mode
- automatic Codex restart preference

Every Tauri command string belongs to a domain adapter under
`apps/desktop/src/features/<domain>/api.ts`. Those adapters use the single
`shared/api/tauri.ts` transport wrapper. UI components and controllers call
those typed functions rather than `invoke` directly. The transport wrapper
must remain a thin pass-through and must not contain domain behavior. Existing
command names and camelCase IPC fields are compatibility contracts and must not
change merely because a file moves.

Current frontend domains:

- `features/codex`: Codex state, diagnostics, restart and external URL actions.
- `features/providers`: provider CRUD, apply, connection test, model discovery,
  and CC Switch provider import.
- `features/official-accounts`: OpenAI Official snapshot/account operations.
- `features/prompts`: local and built-in prompt operations.
- `features/sessions`: session synchronization and deletion.
- `features/skills-mcp`: managed Skills and MCP operations.
- `features/updates`: release fallback check.

Feature-private view types should live with their feature. Cross-feature IPC DTOs
remain in `src/types.ts` until a DTO is used by one feature only.

## Backend

Rust dependency direction is:

```text
Tauri command adapter
  -> domain operation
  -> SQLite, filesystem, platform, or remote adapter
```

`lib.rs` currently hosts command registration and some command adapters, while
existing domain modules implement most business logic:

- `providers/`: saved third-party providers, Official auth/snapshots/accounts,
  live configuration changes, connection testing, and CC Switch import.
- `prompts/`: catalog, managed AGENTS block handling, and SQLite prompt store.
- `sessions/`: catalog, storage discovery, sync, deletion, backup, and rollback.
- `skills_mcp/`: managed MCP configuration and Skills installation/state.
- `live_config.rs`: file lock, checked atomic writes, snapshots and rollback.
- `app_db.rs`: Codex-X SQLite initialization and migrations.
- `paths.rs`: `CODEXX_HOME` and Codex-X application storage root.

New Tauri command adapters should be placed under a command-domain module when
the corresponding domain is changed. A command adapter only deserializes input,
uses `spawn_blocking` for blocking work, adds operation context to errors, and
returns DTOs. Filesystem transactions and database decisions belong below it.

## Data Ownership

`CODEX_HOME` selects one Codex installation/configuration directory. It owns the
live files:

```text
CODEX_HOME/config.toml
CODEX_HOME/auth.json
CODEX_HOME/AGENTS.md
CODEX_HOME/tmp/codex-x-live-config.lock
```

`CODEXX_HOME` selects Codex-X application data, defaulting to `~/.codexx`. It
owns:

```text
CODEXX_HOME/codexx.db                 # providers, prompts, managed Skills/MCP
CODEXX_HOME/official-configs/<hash>.json
CODEXX_HOME/official-accounts/<hash>.json
```

Official snapshot and account file names are SHA-256 hashes of the canonical
`CODEX_HOME` path. Consequently, credentials and selections are isolated between
Codex homes. The provider SQLite store is application-level; the active live
provider is resolved from the selected Codex home's live TOML and auth files.

Browser `localStorage` is only UI preference/cache state. It is never the source
of truth for provider, account, prompt, session, or Skills/MCP note persistence.

## Session Maintenance Desktop Lifecycle

Session synchronization and permanent deletion are backend-owned maintenance
operations. The backend acquires the per-`CODEX_HOME` session maintenance lock,
performs a non-mutating preflight, and only stops Codex Desktop when a mutation
is actually required. After a verified stop it discards pre-stop discovery data,
rediscovers SQLite and rollout storage, and validates the operation again before
writing.

On Windows, Desktop detection is restricted to the `OpenAI.Codex`,
`OpenAI.CodexBeta`, and `OpenAI.ChatGPT-Desktop` package identities. A process is
eligible for graceful close or force termination only when its executable path
is inside the matching package `InstallLocation`. A same-named Codex CLI outside
that package path is never a Desktop target. Stop failure cancels all session
mutation. If Desktop was running, restoration is attempted after both success
and failure; restoration warnings do not change an already completed mutation
into a failed mutation result.

The deletion path retains the official `codex app-server --stdio` API. It is an
independent CLI child bound to the selected `CODEX_HOME`, so it runs after the
Desktop stop and post-stop target validation, before exact local cleanup and
verification.

## Skills/MCP Notes

Skill and MCP notes are Codex-X user metadata. They are stored only in
`CODEXX_HOME/codexx.db` in `skills_mcp_notes`, keyed by canonicalized
`CODEX_HOME`, item kind, and stable item ID. State construction loads notes for
the selected Codex home in one query and attaches them to both managed and
scanned items. Notes must never modify `SKILL.md`, Skill directories,
`config.toml`, MCP JSON, cc-switch storage, or any external source. Empty notes
delete metadata rows; orphan rows may remain so a temporarily missing item can
recover its note when it reappears.

## Provider Data Flow

1. The frontend edits a temporary `SavedProvider` form.
2. `features/providers/api.ts` calls a stable Tauri command.
3. Rust normalizes, validates, and persists the saved profile in `codexx.db`.
4. When activation is requested, `providers/live.rs` snapshots current live
   files, captures current Official state if needed, creates a backup, applies
   checked atomic writes, then builds the returned `CodexState`.
5. The frontend replaces its state from the returned DTO and refreshes derived
   lists.

Rust is authoritative for provider profile identity, TOML normalization,
deduplication, source reconciliation, and live-provider matching. A frontend
identity calculation is display-only and must never decide whether a profile can
be saved, merged, deleted, or activated.

Provider profile identity includes canonical base URL, credential, model, wire
API, and `requires_openai_auth`. Therefore the same endpoint and API key with
different models are distinct profiles. CC Switch reconciliation additionally
uses its stable `source_id`, so a subsequent import updates the same imported
record even if profile fields change.

## Official Account Data Flow

An Official account stores validated OpenAI-only `auth.json` data plus a
sanitized official TOML document. It is never stored in the third-party provider
record. Before switching from Official A to B, Codex-X captures A's current live
refreshed token to its selected account record. Deleting an account records a
reset marker so a deleted account cannot reappear through an older legacy
snapshot.

The older `official-configs` snapshot remains supported for existing users.
Multi-account data is additive and isolated by canonical `CODEX_HOME`.

## Live File Transaction Invariants

All live mutations acquire the per-Codex-home lock and use checked atomic writes.
A write compares the current file with the snapshot observed at operation start;
if another process changed it, Codex-X aborts rather than overwriting it.

The transaction implementation must preserve these rules:

1. Third-party credentials never enter an Official snapshot/account.
2. OpenAI Official credentials are never published while a third-party route is
   active.
3. Official-to-third-party removes/replaces auth before exposing a proxy route.
4. Third-party-to-Official publishes the official route before its credential.
5. A failure restores `config.toml`, `auth.json`, Official snapshot/account
   state, and provider persistence coherently, unless a checked rollback is
   blocked by an external writer. That condition must remain explicit in the
   returned error.
6. Every mutation uses the selected `CODEX_HOME`; no data may cross homes.

`providers/live.rs` is a high-risk module. Future decomposition should first
extract its existing `LiveAuthAction`, write-order selection, snapshots and
rollback into a transaction module; then move third-party switching and Official
account flows one tested responsibility at a time. Do not duplicate transaction
code among switch, account update, account delete, or provider edit paths.

## Extending The Application

Add UI state and controller logic inside the owning feature, not in `main.tsx`.
Add its IPC adapter to the same feature. Add Rust domain behavior below a thin
command adapter, and keep persistence migrations near their actual storage.
Cross-feature coordination belongs in `app/App.tsx` only when no feature can own
it. Shared pure helpers should have a specific domain name; avoid catch-all
`utils`, `helpers`, or global state contexts.
