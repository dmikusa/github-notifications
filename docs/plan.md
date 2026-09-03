# github-notifications: Implementation Plan

A local web app for managing GitHub notifications. It polls the GitHub REST
notifications API, caches everything locally in SQLite, and presents a fast,
filterable, workspace-based UI for triaging issues, pull requests, and
notifications — going beyond what the github.com/notifications UI offers.

## Problem

The GitHub notifications UI at https://github.com/notifications has hard limits:

- Filters are limited in count and composition (no easy org include + repo
  exclude, no saved filter sets beyond a small number).
- There is no search across notifications; the UI only pages through the inbox.
- Notifications about closed/merged PRs clutter the inbox, and there is no way
  to bulk-dismiss them automatically.
- No separation between work and personal activity.
- No batch management of watched repositories.
- Closed/idle items silently fall out of view, which encourages leaving
  notifications unread as a reminder.

## Goals

- **Poll the notifications API** (`GET /notifications`) and cache results
  locally so the UI never re-pages from GitHub.
- **Queue-first UI**: the primary view is open issues & PRs for explicitly
  selected repos; notifications provide the "attention" signal that sorts the
  queue. Nothing leaves the queue until its GitHub state actually changes
  (closed/merged) or the user dismisses it.
- **Workspaces** with separate repo sets and filter sets (e.g. personal vs work).
- **Unlimited, composable saved filters** (org include, repo exclude, state,
  type, age, unread, reason, etc.).
- **Bulk operations**: select many, then mark read/unread, dismiss, mute, open
  in GitHub, watch/unwatch repos.
- **Auto-dismiss (opt-in)**: automatically mark threads read when their subject
  PR is closed and merged.
- **Watch/subscription management** with batch changes.

## Non-Goals (current phase)

- Event-driven ingestion via webhooks or a GitHub App (there is no webhook for
  the notification inbox; see Key Decisions — Auth).
- Mobile clients, multiple users, hosted/multi-tenant deployment.
- Advanced issue/PR lifecycle actions beyond linking out to GitHub.

## Why Rust?

- **Single binary**: the web UI is embedded into the binary via `rust-embed`;
  users run one executable with no runtime or build step.
- **Low-dependency, long-lived**: a deliberately small, stable dependency set
  (axum, tokio, reqwest, rusqlite) keeps maintenance burden low over the app's
  lifetime.
- **Safe concurrency**: the background sync loop and the HTTP server share
  state safely.
- **Distribution**: cargo-dist produces per-platform binaries and Homebrew
  formulae; the Paketo Rust buildpack produces an OCI image.

## Platform Choice

A **local daemon + browser UI**:

- The daemon polls GitHub, caches to SQLite, and serves a JSON API + embedded
  static frontend on `http://localhost`.
- The browser is the richest environment for the interaction this app needs:
  checkbox multi-select, bulk action bars, sortable/groupable tables, and
  easy filter pickers.
- Iteration is fast: no build tooling for the frontend, hot reload via a simple
  refresh.
- "Turn it on, sync, shut it down": start the binary, use the UI, Ctrl+C.

The frontend intentionally avoids an npm/node build cycle. It is plain
HTML/CSS/JS plus **HTMX** for server-rendered fragment swaps. JS is kept in
small, ordered modules that the daemon concatenates into a single `/app.js` at
request time (no minification, easier to debug).

## Architecture Overview

See `docs/architecture.md` for the full C4 model. In brief:

```
GitHub API  <--poll/since/ETag-->  Local daemon (Rust)
                                     |-- SQLite cache (threads, issues, repos)
                                     `-- HTTP server on localhost
                                           |-- JSON API (/api/*)
                                           `-- embedded UI (index.html, /app.js)
Browser UI <----HTMX fragments + JSON----  localhost:PORT
```

### Sync loop

One global loop, **all workspaces, deduped** (deliberately not scoped to the
active workspace):

1. **Notifications** (`GET /notifications?since=<last>&per_page=50`, paged,
   conditional via `If-None-Match`): first run does a full catch-up, then
   incremental via `since`. Stores threads in SQLite. Respects the
   `X-Poll-Interval` response header and defaults to a configurable interval
   (default 5 min).
2. **Issues/PRs** for every repo in every workspace's repo sets (deduped):
   `GET /repos/{owner}/{repo}/issues?state=open` and
   `GET /repos/{owner}/{repo}/pulls?state=open`, both conditional
   (`If-None-Match`). 304 responses do not consume rate limit, so refresh is
   cheap (default 10 min).
3. **Watch/subscription state** (`GET /user/subscriptions`) on a slow cadence,
   for the watch manager and to detect unwatches.

Manual sync is available via `POST /api/sync`.

### Data model

Config (`config.toml`, gitignored):

```toml
[github]
auth_provider = "pat"           # pat | gh-token | oauth-device
auth_token = ""                 # PAT (or use GITHUB_TOKEN env)
oauth_client_id = ""            # for oauth-device
poll_interval_seconds = 300
repo_refresh_interval_seconds = 600

[[workspaces]]
name = "personal"
auto_dismiss_closed_merged = false
  [[workspaces.repo_sets]]
  name = "paketo"
  repos = ["paketo-buildpacks/abc", "paketo-community/def"]
```

- **Workspace** = a named lens with repo sets, saved filters, and the
  auto-dismiss toggle.
- **Repo set** = an explicit list of repos. Orgs are *not* wildcards; every
  tracked repo is explicitly selected (a UI picker will browse orgs and write
  back explicit lists). New org repos never auto-join.

SQLite cache (`~/.local/share/github-notifications/data.db`):

- `repos` — tracked repos, watch status, last refresh.
- `issues` — issues and PRs (`kind` = issue|pr), full state incl. `merged_at`.
- `threads` — notification threads, joined to issues via subject API URL.
- `sync_state` — last sync times and per-resource ETags.

### Queue semantics

The queue is built from `issues` for tracked repos. Each item joins to its
latest thread; unread/updated/reason threads *bubble* active discussions to the
top. Items persist in the queue until `state` becomes closed (or the user
explicitly dismisses/snoozes), regardless of GitHub inbox read state.

## Key Decisions

### Auth

The GitHub notifications endpoints are documented as accepting only classic
PATs; empirical testing confirmed OAuth app tokens with `repo` scope also work
(gh's own token returns 200). Fine-grained PATs cannot be used (no
"notifications" permission category exists). Auth is abstracted behind an
`AuthProvider` trait with three implementations:

1. **Classic PAT** (`auth_provider = "pat"`) — token in config or
   `GITHUB_TOKEN` env. Works for everything.
2. **gh-token reuse** (`gh-token`) — runs `gh auth token` at startup, caches,
   refreshes on 401. Zero PAT management; gh's default `repo` scope covers the
   inbox.
3. **OAuth device flow** (`oauth-device`) — browser authorize flow, stores the
   token locally; useful without gh installed.

Setup steps and scope requirements are documented in `SETUP.md`.

### Sync all workspaces, dedupe

Scoping sync to the active workspace is rejected: notifications are global
anyway (no repo filter exists), and per-workspace catch-up on switch would cause
stale data and added complexity. The global loop keeps every workspace fresh;
the UI filters per workspace.

### No org wildcards

Repo sets are explicit repo lists only. New repos in an org never auto-join a
workspace.

### Auto-dismiss is opt-in

`auto_dismiss_closed_merged = false` by default per workspace. When enabled,
after a repo refresh the daemon marks threads read when the subject PR is
closed and merged. Queue items are unaffected (they follow GitHub state).

### Frontend conventions

- No build step, no minification, no package manager.
- Server-side filtering/sorting/grouping in Rust; HTMX swaps table bodies and
  filter options; JS handles selection state and API calls.
- JS modules are concatenated in dependency order (`src/assets.rs::JS_BUNDLE`).
- Each JS file carries a `// js/<path>:` header that tests verify.

## API Surface (localhost)

- `GET /api/state` — version, workspaces, auth status, sync status.
- `GET /api/workspaces/{id}/queue` — issues/PRs joined to threads; sort, group,
  filter params; JSON + `tbody` HTML fragment variants.
- `GET /api/workspaces/{id}/inbox` — notification threads (same pattern).
- `POST /api/workspaces/{id}/notifications/mark-read` (bulk ids),
  mark-all-read, mute (unsubscribe thread).
- `POST /api/repos/{owner}/{repo}/watch|unwatch|ignore`.
- `POST /api/sync` — manual sync trigger.
- `GET /api/workspaces/{id}/reposets`, `GET /api/orgs/{org}/repos` — repo
  picker data.
- Static: `/` (index.html), `/app.js` (concatenated JS), `/app.css`.

## Phases

Each phase ends with tests passing and a single git commit. Phases are reviewed
with the human before proceeding.

### Phase 0 — Scaffold & repo infrastructure
- Git repo, GPL-2.0-or-later license, `AGENTS.md`, `rust-toolchain.toml`.
- Cargo project: config loading (TOML), SQLite schema, embedded UI,
  `/api/state`, static serving + JS concat.
- Container-friendly env handling (`PORT`, `GHNOTIFY_DATA_DIR`, `GITHUB_TOKEN`).
- Docs: `docs/plan.md`, `docs/architecture.md`, `SETUP.md`.
- CI: `cargo fmt`/`clippy`/`test` on PR + zizmor job.
- Renovate config + workflow.
- cargo-dist init (workflow, installers, targets).

### Phase 1 — Auth
- `AuthProvider` trait + PAT, gh-token, OAuth device flow implementations.
- Startup scope validation (`X-OAuth-Scopes`), 401 refresh/re-auth surfacing,
  SAML SSO hint on 403.
- Token storage (gitignored, private permissions).

### Phase 2 — Sync engine
- Global loop: notifications (since/ETag), repo issues/PRs (conditional
  requests), watch/subscription state.
- ETag cache in `sync_state`, rate-limit surfacing, manual sync endpoint.
- Opt-in auto-dismiss of closed+merged PR threads.

### Phase 3 — UI
- Nav + Queue view (sortable/groupable table, checkbox selection, bulk bar),
  Inbox view, workspaces + saved filters, watch manager with bulk changes.

### Phase 4 — Extras
- Repo set management UI (org browser → checkbox → writes config), config
  write-back, GraphQL-vs-REST optimization spike.

### Phase 5 — Distribution
- OCI image workflow (Paketo community Rust buildpack), cargo-dist release
  validation, README polish.

## Testing Strategy

- Unit tests in each module (config parsing, DB schema/round-trips, auth
  token resolution, JS bundle integrity).
- API integration tests via `tower::ServiceExt::oneshot` against an in-memory
  temp database.
- Sync engine tests against a mocked GitHub HTTP endpoint (deterministic
  fixtures), covering incremental sync, 304 handling, and auto-dismiss.
- CI runs `cargo fmt -- --check`, `cargo clippy -- -D warnings`, and
  `cargo test` on every PR.

## Release & Distribution

- **cargo-dist**: multi-target binaries + Homebrew formula
  (tap `dmikusa/homebrew-tap`), GitHub-hosted release artifacts.
- **OCI image**: built with the Paketo community Rust buildpack
  (`paketocommunity/rust`); the daemon binds `0.0.0.0:$PORT` in the container.

## Process Rules

1. Every phase must include tests for all new functionality.
2. All tests must pass before a phase is complete.
3. Each phase ends with a single git commit.
4. No work may be deferred without explicit human confirmation.
5. After completing a phase, present a review to the human before proceeding.
6. Before every commit and push, run `cargo fmt` then
   `cargo clippy -- -D warnings` and ensure both pass clean.