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

/// PR detail used for the auto-dismiss check and inbox subject status
/// (`GET /repos/{o}/{r}/pulls/{n}`).
#[derive(Debug, Deserialize)]
pub struct GithubPullRequest {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub merged_at: Option<String>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub user: Option<GithubUser>,
}

/// A check run or workflow run (`GET .../check-runs/{id}` / `.../actions/runs/{id}`),
/// used to show CI status on inbox threads.
#[derive(Debug, Deserialize)]
pub struct CheckRun {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
}

/// A page of workflow runs from `GET /repos/{o}/{r}/actions/runs`.
#[derive(Debug, Deserialize)]
pub struct ActionsRuns {
    #[serde(default)]
    pub workflow_runs: Vec<ActionsRun>,
}

#[derive(Debug, Deserialize)]
pub struct ActionsRun {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub created_at: String,
}

/// A repository from `GET /user/subscriptions`.
#[derive(Debug, Deserialize)]
pub struct WatchedRepo {
    pub full_name: String,
    pub html_url: String,
}

/// A page of issue/PR search results (`GET /search/issues`), used to find pull
/// requests awaiting review.
#[derive(Debug, Deserialize)]
pub struct SearchResults {
    #[serde(default)]
    pub items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
pub struct SearchItem {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub user: Option<GithubUser>,
    /// API URL of the owning repository, e.g.
    /// `https://api.github.com/repos/o/r`.
    #[serde(default)]
    pub repository_url: String,
}

impl SearchItem {
    /// The `owner/repo` full name derived from `repository_url`.
    pub fn repo_full_name(&self) -> Option<String> {
        let rest = self.repository_url.split("/repos/").nth(1)?;
        let mut parts = rest.split('/');
        let owner = parts.next().filter(|s| !s.is_empty())?;
        let name = parts.next().filter(|s| !s.is_empty())?;
        Some(format!("{owner}/{name}"))
    }
}

/// The `data` shape of a GraphQL discussions query.
#[derive(Debug, Deserialize)]
pub struct DiscussionPage {
    #[serde(default)]
    pub repository: Option<DiscussionRepository>,
}

#[derive(Debug, Deserialize)]
pub struct DiscussionRepository {
    #[serde(default)]
    pub discussions: DiscussionConnection,
}

#[derive(Debug, Default, Deserialize)]
pub struct DiscussionConnection {
    #[serde(default)]
    pub nodes: Vec<DiscussionNode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscussionNode {
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub author: Option<GithubUser>,
    #[serde(default)]
    pub category: Option<DiscussionCategory>,
    /// The repository the discussion belongs to, when returned by a search.
    #[serde(default)]
    pub repository: Option<DiscussionRepoRef>,
}

#[derive(Debug, Deserialize)]
pub struct DiscussionCategory {
    pub name: String,
}

/// The repository reference attached to a discussion search result.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscussionRepoRef {
    pub name_with_owner: String,
}

/// The `data` shape of a GraphQL discussion search (organization-level
/// discussions).
#[derive(Debug, Deserialize)]
pub struct DiscussionSearchPage {
    #[serde(default)]
    pub search: DiscussionSearch,
}

#[derive(Debug, Default, Deserialize)]
pub struct DiscussionSearch {
    #[serde(default)]
    pub nodes: Vec<DiscussionNode>,
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

    #[test]
    fn parses_search_results_and_repo_name() {
        let results: SearchResults = serde_json::from_str(
            r#"{"items":[{
                "number": 7,
                "title": "a pr",
                "html_url": "https://github.com/o/r/pull/7",
                "updated_at": "2026-09-03T12:00:00Z",
                "created_at": "2026-09-01T12:00:00Z",
                "user": {"login": "alice"},
                "repository_url": "https://api.github.com/repos/o/r"
            }]}"#,
        )
        .expect("search results");
        assert_eq!(results.items.len(), 1);
        assert_eq!(results.items[0].repo_full_name().as_deref(), Some("o/r"));
        assert_eq!(
            results.items[0].user.as_ref().map(|u| u.login.as_str()),
            Some("alice")
        );
    }

    #[test]
    fn parses_discussion_search_results() {
        let page: DiscussionSearchPage = serde_json::from_str(
            r#"{"search":{"nodes":[{
                "number": 447,
                "title": "a feedback discussion",
                "url": "https://github.com/orgs/o/discussions/447",
                "createdAt": "2026-10-21T14:13:59Z",
                "updatedAt": "2026-10-22T08:16:38Z",
                "author": {"login": "alice"},
                "category": {"name": "Q&A"},
                "repository": {"nameWithOwner": "o/feedback"}
            }]}}"#,
        )
        .expect("discussion search");
        assert_eq!(page.search.nodes.len(), 1);
        let node = &page.search.nodes[0];
        assert_eq!(node.number, 447);
        assert_eq!(
            node.repository.as_ref().map(|r| r.name_with_owner.as_str()),
            Some("o/feedback")
        );
    }

    #[test]
    fn parses_discussion_nodes() {
        let page: DiscussionPage = serde_json::from_str(
            r#"{"repository":{"discussions":{"nodes":[{
                "number": 4,
                "title": "a discussion",
                "url": "https://github.com/o/r/discussions/4",
                "createdAt": "2026-09-01T00:00:00Z",
                "updatedAt": "2026-09-03T00:00:00Z",
                "author": {"login": "bob"},
                "category": {"name": "General"}
            }]}}}"#,
        )
        .expect("discussion page");
        let nodes = page.repository.expect("repository").discussions.nodes;
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].number, 4);
        assert_eq!(
            nodes[0].category.as_ref().map(|c| c.name.as_str()),
            Some("General")
        );
        assert_eq!(
            nodes[0].author.as_ref().map(|a| a.login.as_str()),
            Some("bob")
        );
    }
}
