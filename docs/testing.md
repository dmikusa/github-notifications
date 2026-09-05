# Testing

## Running the suite

- `cargo test` — unit + integration tests. No network access; GitHub API calls
  are served by mock servers. All tests must pass before a phase is complete.
- `cargo online-checks` — runs the **online** ignored-test group (live GitHub
  API). Alias for `cargo test -- --ignored online_`. Requires `GITHUB_TOKEN` or
  a logged-in `gh`.
- `cargo test -- --ignored <prefix>` — run a specific group of ignored tests.
- `cargo test -- --ignored` — run every ignored test (all groups).

## Ignored test groups

Tests that should not run in normal CI (they need a live API, a long time, or
other prerequisites) are marked `#[ignore]`. Because stock `cargo test` can
only select ignored tests all-at-once or by substring, we group them by a
**name prefix** — the prefix is the group's identifier.

| Group | Test-name prefix | Run with |
| --- | --- | --- |
| online | `online_` | `cargo online-checks` |

### Rules

- Every ignored test's name starts with its group prefix (e.g.
  `online_live_sync_polls_github`).
- An ignored test must **skip cleanly** (return normally with a message) when
  its prerequisites are absent, so `-- --ignored` runs are safe on any machine.
- Prefixes must be short and unique.

### Adding a new group

1. Name the test with the new prefix, e.g. `slow_benchmark`.
2. Mark it `#[ignore]`, ideally with a reason explaining how to run it.
3. Make it skip cleanly when its prerequisites are missing.
4. Add a cargo alias in `.cargo/config.toml`:
   ```toml
   [alias]
   slow-checks = "test -- --ignored slow_"
   ```
5. Add a row to the group table above and mention the command in `AGENTS.md`.

## Online checks

`tests/smoke.rs` runs one full sync pass against the live GitHub API and
asserts that notifications and repositories are cached locally. It uses
`GITHUB_TOKEN` if set, otherwise the `gh` CLI's token (skips if neither is
available). The repo under test can be overridden with `GH_NOTIFY_SMOKE_REPO`.