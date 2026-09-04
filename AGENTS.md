# Agent Instructions

This project follows the implementation plan in `docs/plan.md`.

Before working on any task, read the plan document to understand:
- The architecture and design decisions
- The current phase of implementation
- The process rules for phase completion

## Rules

1. Never push/merge to `main` directly. Always use a pull request.
2. You are not authorized to merge PRs. Only the human can merge.

## Quick Reference

- **Build**: `cargo build`
- **Test**: `cargo test`
- **Lint**: `cargo clippy -- -D warnings`
- **Format**: `cargo fmt`
- **Run**: `cargo run -- --config ./config.toml --bind 127.0.0.1:8080`
- **Coverage**: `mkdir -p target/coverage && cargo llvm-cov --lcov --output-path target/coverage/lcov.info`
- **Online checks**: `cargo online-checks` (runs the ignored live-GitHub smoke test via `cargo test -- --ignored`; requires `GITHUB_TOKEN` or a logged-in `gh`)

## Process Rules

1. Every phase must include tests for all new functionality
2. All tests must pass before a phase is complete
3. Each phase ends with a single git commit
4. No work may be deferred without explicit human confirmation
5. After completing a phase, present a review to the human before proceeding
6. Before every commit and push, run `cargo fmt` then `cargo clippy -- -D warnings` and ensure both pass clean