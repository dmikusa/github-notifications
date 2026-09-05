# github-notifications

![License](https://img.shields.io/badge/license-GPL--2.0--or--later-blue.svg)
[![CI](https://github.com/dmikusa/github-notifications/actions/workflows/ci.yml/badge.svg)](https://github.com/dmikusa/github-notifications/actions/workflows/ci.yml)

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
- **Repo set management**: browse an org, checkbox-select repos, and save a
  repo set — all from the UI (Settings tab).
- **Opt-in auto-dismiss** of closed+merged PR notifications.
- **Single binary**: the web UI is embedded; no runtime or build step.

## Quick start

```console
$ github-notifications
# First run: creates ~/.config/github-notifications/config.toml (a commented
# example) and exits. Add a workspace + repo set to it, then run again.
$ github-notifications
# Serves the UI at http://127.0.0.1:8080 (bind with --bind, $HOST, or $PORT)
```

Follow [docs/setup.md](docs/setup.md) to configure GitHub authentication
(classic PAT, gh-token reuse, or OAuth device flow) and add workspaces/repo
sets.

## Container

Prebuilt images are published to GHCR (`ghcr.io/dmikusa/github-notifications`)
when a `v*` tag is pushed, built with the Paketo Rust buildpack.

```console
$ docker run -p 8080:8080 \
    -e HOST=0.0.0.0 \
    -e GHNOTIFY_CONFIG=/app/config.toml \
    -v "$PWD/config.toml:/app/config.toml" \
    -e GHNOTIFY_DATA_DIR=/data \
    -v notify-data:/data \
    ghcr.io/dmikusa/github-notifications:latest
```

The container listens on `127.0.0.1` by default; set `HOST=0.0.0.0` to expose
it beyond loopback. Point `GHNOTIFY_CONFIG` at a mounted config file that has a
workspace + repo set, and mount a volume for the SQLite data directory.

## Development

```console
$ cargo build
$ cargo run -- --bind 127.0.0.1:8080   # opens the browser automatically
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