# Architecture (C4 Model)

## C1 — System Context

```mermaid
flowchart TB
  User(["<b>User</b><br/>Developer or operator"])
  style User fill:#1168bd,color:#fff,stroke:#1168bd

  subgraph Ext["External Systems"]
    style Ext fill:#eee,stroke:#999,color:#333
    GitHub["GitHub REST API"]
    Browser["Browser"]
    Fs["Local Filesystem"]
  end

  subgraph App["github-notifications"]
    style App fill:#e8f4f8,stroke:#08c,color:#005
    Daemon["<b>Daemon</b><br/>Local Rust HTTP server + sync loop"]
  end

  User -->|"uses browser UI"| Browser
  Browser -->|"HTTP on localhost"| Daemon
  Daemon -->|"poll /notifications, repos, issues"| GitHub
  Daemon -->|"read/write"| Fs
  Daemon -->|"open subject in GitHub"| GitHub
```

## C2 — Containers

```mermaid
flowchart TB
  User(["<b>User</b>"])
  style User fill:#1168bd,color:#fff,stroke:#1168bd

  subgraph Ext["External Systems"]
    style Ext fill:#eee,stroke:#999,color:#333
    GitHub["GitHub REST API"]
    Browser["Browser"]
    Fs["Local Filesystem"]
  end

  subgraph Daemon["github-notifications daemon"]
    style Daemon fill:#e8f4f8,stroke:#08c,color:#005
    Cfg["<b>Config</b><br/>TOML loader"]
    DB["<b>Database</b><br/>rusqlite / SQLite"]
    Auth["<b>Auth</b><br/>AuthProvider trait"]
    Sync["<b>Sync Loop</b><br/>tokio background task"]
    API["<b>API</b><br/>axum router"]
    Assets["<b>Assets</b><br/>rust-embed UI"]
  end

  subgraph UI["Browser frontend"]
    style UI fill:#d4e6f1,stroke:#5b9bd5
    Html["index.html + app.css"]
    Js["js modules → /app.js"]
    Htmx["HTMX fragments"]
  end

  User --> Browser
  Browser --> Html
  Html --> Js
  Browser -->|"JSON + HTML"| API
  Htmx --> API
  API --> DB
  API --> Assets
  Sync --> DB
  Sync --> Auth
  Auth --> GitHub
  Sync --> GitHub
  Cfg --> DB
  Cfg --> Sync
  Daemon --> Fs
```

## C3 — Components

### Sync Engine

```mermaid
flowchart TB
  subgraph Sync["Sync Loop"]
    style Sync fill:#e8f4f8,stroke:#08c,color:#005
    NS["<b>NotificationSync</b><br/>GET /notifications?since=&lt;last&gt;<br/>If-None-Match + X-Poll-Interval"]
    RS["<b>RepoRefresh</b><br/>open issues + open PRs per repo<br/>conditional (ETag) requests"]
    WS["<b>WatchSync</b><br/>GET /user/subscriptions"]
    ET["<b>ETagStore</b><br/>sync_state table"]
    AD["<b>AutoDismiss</b><br/>opt-in; mark closed+merged PR threads read"]
    Sched["<b>Scheduler</b><br/>intervals + manual trigger"]
  end

  Sched --> NS
  Sched --> RS
  Sched --> WS
  NS --> ET
  RS --> ET
  RS --> AD
  NS -->|"threads"| DB
  RS -->|"issues/prs"| DB
  WS -->|"watched"| DB
```

### Auth Providers

```mermaid
flowchart TB
  subgraph A["AuthProvider trait"]
    style A fill:#e8f4f8,stroke:#08c,color:#005
    PAT["<b>ClassicPat</b><br/>config / GITHUB_TOKEN"]
    GH["<b>GhToken</b><br/>gh auth token, cached,<br/>refresh on 401"]
    OAuth["<b>OAuthDevice</b><br/>device flow, local token store"]
  end
  A -->|"Bearer token"| GitHub
  A -->|"scope validation"| Chk["X-OAuth-Scopes check"]
```

### API Surface

```mermaid
flowchart TB
  subgraph Api["axum router"]
    style Api fill:#e8f4f8,stroke:#08c,color:#005
    ST["GET /api/state"]
    QU["GET /api/workspaces/{id}/queue"]
    IN["GET /api/workspaces/{id}/inbox"]
    OP["POST .../notifications/mark-read | mute"]
    RW["POST /api/repos/{o}/{r}/watch|unwatch"]
    SY["POST /api/sync"]
    STATIC["/ , /app.js , /app.css"]
  end
  ST --> Cfg
  QU --> DB
  IN --> DB
  OP --> Sync
  RW --> Sync
  SY --> Sync
  STATIC --> Assets
```

## Notes

- The daemon and browser run on the same machine; all `/api/*` traffic is
  loopback-only by default (`127.0.0.1`). In a container the daemon binds
  `0.0.0.0:$PORT`.
- The GitHub API is the only external dependency; there are no databases or
  services beyond the local SQLite file.
- The UI is embedded at compile time (`rust-embed`), so the daemon binary is
  self-contained.