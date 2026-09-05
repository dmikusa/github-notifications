# github-notifications

A local web app for managing GitHub notifications. It polls the GitHub
notifications API, caches everything locally in SQLite, and gives you fast,
filterable, workspace-based views of your open issues, PRs, and notification
threads — beyond what github.com/notifications supports.

## Highlights

- **Queue-first triage**: open issues & PRs for the repos you select; active
  discussions bubble to the top. Nothing leaves the queue until it is
  closed/merged or you dismiss it.
- **Workspaces**: separate repo sets and filter sets (e.g. personal vs work).
- **Unlimited saved filters**: org/repo include & exclude, state, type, age,
  unread, reason, and more.
- **Bulk actions**: select many, then mark read/unread, dismiss, mute, watch or
  unwatch repos.
- **Opt-in auto-dismiss** of closed+merged PR notifications.
- **Single binary**: the web UI is embedded; no runtime or build step.

## Quick start

```console
$ github-notifications
# creates ~/.config/github-notifications/config.toml and a local SQLite cache,
# then serves the UI at http://127.0.0.1:8080
```

Follow [docs/setup.md](docs/setup.md) to configure GitHub authentication
(classic PAT, gh-token reuse, or OAuth device flow) and add workspaces/repo
sets.

## Development

```console
$ cargo build
$ cargo run -- --bind 127.0.0.1:8080 --open
$ cargo test
```

Run `cargo fmt` and `cargo clippy -- -D warnings` before committing.

### Checks

```console
$ cargo test              # unit + integration tests (no network)
$ cargo online-checks     # live GitHub smoke test (needs GITHUB_TOKEN or a logged-in gh)
```

Online and other non-default tests are grouped by a name-prefix convention;
see [docs/testing.md](docs/testing.md) for the groups and how to add one.

## Architecture & plan

- [docs/architecture.md](docs/architecture.md) — C4 architecture diagrams.
- [docs/plan.md](docs/plan.md) — implementation plan and phases.

## License

GPL-2.0-or-later. See [LICENSE](LICENSE).