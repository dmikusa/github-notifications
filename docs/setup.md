# Setup

## Authentication overview

github-notifications reads your GitHub notification inbox through the REST
endpoint `GET /notifications`. Those endpoints have a quirk:

- They only accept tokens carrying the `notifications` **or** `repo` scope.
- **Fine-grained PATs cannot be used** — there is no "notifications"
  permission category for fine-grained tokens, and GraphQL has no
  notifications query at all.
- Although the docs say "classic PAT only," an **OAuth app token with `repo`
  scope works** (verified: `gh`'s own OAuth token fetches the inbox fine).
  That is why the OAuth device flow is supported.

Three auth providers are available, configured with
`auth_provider = "pat" | "gh-token" | "oauth-device"` in `config.toml`.

## Provider 1 — Classic PAT (default)

1. Go to <https://github.com/settings/tokens> → **Generate new token** →
   **Generate new token (classic)**.
2. Name it (e.g. `github-notifications`) and set an expiration
   (recommend ≤ 90 days; you will need to rotate it).
3. Select scopes:
   - `notifications` — **required** for the inbox: read notifications, mark
     threads read, watch/unwatch repos, manage thread subscriptions.
   - `repo` — **required to read issues and PRs on private repositories**.
     The `notifications` scope alone only covers the inbox. Omit `repo` only
     if you will use the app exclusively with public repos.
   - `read:org` — only if you want org/repo browsing in the UI picker.
4. If a work org enforces **SAML SSO**, authorize the token for that org after
   creating it (follow the SSO prompt / "Configure SSO").
5. Save the token:
   - in `config.toml` under `[github] auth_token`, or
   - set the `GITHUB_TOKEN` environment variable.
   `config.toml` is gitignored and the daemon writes it with `0600`
   permissions when a token is present.

> Security note: `repo` is a broad scope (full access to your repositories).
> This is a GitHub-side tradeoff — the narrow `notifications` scope cannot
> reach private-repo issue/PR data. If you can live with public-repo data
> only, use just `notifications`.

## Provider 2 — gh-token reuse (zero-setup path)

- Prerequisite: the GitHub CLI is installed and logged in
  (`gh auth login`, any method).
- Set `auth_provider = "gh-token"` in `config.toml`.
- The daemon runs `gh auth token` at startup, caches the token, and re-fetches
  it on a `401`. `gh` auto-refreshes its token, so you never manage a PAT.
- `gh`'s default token includes the `repo` scope, which satisfies the
  notifications endpoints (verified: HTTP 200).

## Provider 3 — OAuth device flow

1. Create an OAuth App at
   <https://github.com/settings/developers> → **New OAuth App**.
   - Application name: `github-notifications`
   - Homepage URL: `http://localhost`
   - Authorization callback URL: `http://localhost/callback`
     (a required field; unused by the device flow)
2. Copy the **Client ID**. No client secret is needed — the device flow is a
   public client.
3. In `config.toml`:
   ```toml
   [github]
   auth_provider = "oauth-device"
   oauth_client_id = "Iv1.xxxxxxxxxxxxxxxx"
   ```
4. On first run, the daemon prints a verification URL and code (and opens the
   browser when possible). Approve the GitHub authorize screen. Requested
   scopes are `notifications`, `repo`, `read:org` (configurable later).
5. The daemon polls until authorized, stores the access token locally
   (gitignored, `0600`), and starts syncing.

Notes:
- If your OAuth app settings include an "Enable Device Flow" toggle, ensure it
  is on.
- If you later revoke or reduce scopes, the daemon detects `401`/`403` and
  re-prompts.
- If the device-flow token lacks `repo` scope it can still manage the inbox,
  but private-repo issue/PR data will be unavailable.

## Configuration reference

| Key | Default | Meaning |
| --- | --- | --- |
| `github.auth_provider` | `pat` | `pat` \| `gh-token` \| `oauth-device` |
| `github.auth_token` | empty | Classic PAT (or use `GITHUB_TOKEN`) |
| `github.oauth_client_id` | empty | OAuth app client ID (device flow) |
| `github.poll_interval_seconds` | `300` | Notification poll interval |
| `github.repo_refresh_interval_seconds` | `600` | Issue/PR refresh interval |
| `workspaces[].name` | — | Workspace display name |
| `workspaces[].auto_dismiss_closed_merged` | `false` | Auto-mark closed+merged PR threads read |
| `workspaces[].repo_sets[].name` | — | Repo set name |
| `workspaces[].repo_sets[].repos` | `[]` | Explicit `owner/repo` list |

On first run the daemon writes a commented example config to
`~/.config/github-notifications/config.toml` (or `$XDG_CONFIG_HOME`). The
plain equivalent, without comments:

```toml
[github]
auth_provider = "pat"
auth_token = ""
oauth_client_id = ""
poll_interval_seconds = 300
repo_refresh_interval_seconds = 600

[[workspaces]]
name = "personal"
auto_dismiss_closed_merged = false

[[workspaces.repo_sets]]
name = "open-source"
repos = [
  "paketo-buildpacks/paketo",
  "paketo-community/anthology",
]

[[workspaces.repo_sets]]
name = "work"
repos = ["your-company/tooling"]
```

## Environment variables

| Variable | Purpose |
| --- | --- |
| `GITHUB_TOKEN` | Classic PAT override when `auth_token` is empty |
| `HOST` | Listen address override (default `127.0.0.1`); use `0.0.0.0` to expose the server beyond loopback |
| `PORT` | Listen port override (default `8080`) |
| `GHNOTIFY_DATA_DIR` | Override the SQLite data directory |
| `RUST_LOG` | Log level (e.g. `debug`, `info`) |

The server listens on `127.0.0.1:8080` by default. Set `HOST=0.0.0.0` (with
`PORT` if desired) to make it reachable from other machines, e.g. in a
container.