use serde::Deserialize;

/// A notification thread as returned by `GET /notifications`.
#[derive(Debug, Deserialize)]
pub struct NotificationThread {
    pub id: String,
    pub unread: bool,
    pub reason: String,
    pub updated_at: String,
    pub last_read_at: Option<String>,
    pub subject: ThreadSubject,
    /// The repository that triggered the notification. Nullable for some
    /// notification types.
    pub repository: Option<ThreadRepository>,
    /// API url of the thread itself (e.g. `.../notifications/threads/123`).
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct ThreadSubject {
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// API url of the subject (issue/PR/discussion/...). Null for some types
    /// (e.g. `CheckSuite` notifications).
    pub url: Option<String>,
    pub latest_comment_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ThreadRepository {
    pub full_name: String,
    pub html_url: String,
}

/// An issue or pull request from `GET /repos/{o}/{r}/issues`. Pull requests
/// are distinguished by the presence of `pull_request`.
#[derive(Debug, Deserialize)]
pub struct GithubIssue {
    pub id: u64,
    pub number: u64,
    pub title: String,
    pub state: String,
    pub user: Option<GithubUser>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub html_url: String,
    pub url: String,
    pub pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct GithubUser {
    pub login: String,
}

/// PR detail used for the auto-dismiss check (`GET /repos/{o}/{r}/pulls/{n}`).
#[derive(Debug, Deserialize)]
pub struct GithubPullRequest {
    pub state: String,
    pub merged_at: Option<String>,
}

/// A repository from `GET /user/subscriptions`.
#[derive(Debug, Deserialize)]
pub struct WatchedRepo {
    pub full_name: String,
    pub html_url: String,
}

/// The authenticated user's subscription for a repository
/// (`GET /repos/{owner}/{repo}/subscription`).
#[derive(Debug, Deserialize)]
pub struct RepoSubscription {
    #[serde(default)]
    pub subscribed: bool,
    #[serde(default)]
    pub ignored: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_notification_thread() {
        let raw = r#"{
            "id": "1:12345",
            "unread": true,
            "reason": "mention",
            "updated_at": "2026-09-03T12:00:00Z",
            "last_read_at": null,
            "subject": {
                "title": "hello",
                "type": "PullRequest",
                "url": "https://api.github.com/repos/o/r/pulls/7",
                "latest_comment_url": null
            },
            "repository": { "full_name": "o/r", "html_url": "https://github.com/o/r" },
            "url": "https://api.github.com/notifications/threads/9876"
        }"#;
        let t: NotificationThread = serde_json::from_str(raw).expect("parse");
        assert_eq!(t.id, "1:12345");
        assert!(t.unread);
        assert_eq!(t.subject.kind, "PullRequest");
        assert_eq!(t.repository.as_ref().expect("repo").full_name, "o/r");
    }

    #[test]
    fn parses_issue_and_pull_request() {
        let issue: GithubIssue = serde_json::from_str(
            r#"{"id":1,"number":3,"title":"x","state":"open","user":{"login":"a"},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","closed_at":null,"html_url":"https://github.com/o/r/issues/3","url":"https://api.github.com/repos/o/r/issues/3"}"#,
        )
        .expect("issue");
        assert!(issue.pull_request.is_none());

        let pr: GithubIssue = serde_json::from_str(
            r#"{"id":2,"number":5,"title":"y","state":"open","user":{"login":"b"},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","closed_at":null,"html_url":"https://github.com/o/r/pull/5","url":"https://api.github.com/repos/o/r/pulls/5","pull_request":{"url":"https://api.github.com/repos/o/r/pulls/5"}}"#,
        )
        .expect("pr");
        assert!(pr.pull_request.is_some());
    }

    #[test]
    fn parses_pull_request_detail() {
        let pr: GithubPullRequest =
            serde_json::from_str(r#"{"state":"closed","merged_at":"2026-02-01T00:00:00Z"}"#)
                .expect("pr");
        assert_eq!(pr.state, "closed");
        assert!(pr.merged_at.is_some());
    }
}
