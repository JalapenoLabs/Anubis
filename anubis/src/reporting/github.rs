//! Filing to GitHub issues, over the REST API.
//!
//! [`GitHub`] implements [`Tracker`] with four calls: open an issue, search for
//! one carrying a fingerprint, read whether one is open, and comment on one.
//! It does not hold a token. It asks its [`Credentials`] for one on every call,
//! because the token belongs to the application: an operator pastes it into a
//! settings page, it is sealed in the application's own table, and it can be
//! rotated between two calls. [`Fixed`] is the credentials for an application
//! that configures one token at boot instead.
//!
//! # The token it needs
//!
//! A fine-grained personal access token, scoped to the one repository issues
//! are filed in, with **Issues: read and write** and nothing else (GitHub adds
//! **Metadata: read** to every fine-grained token by itself). Reading whether
//! an issue is open and searching for one are both covered by that grant.
//!
//! # What it tells the credentials
//!
//! After every call it reports how the token fared, through
//! [`Credentials::record`]: accepted, or refused with a sentence that names the
//! status and what to do about it. That is what lets a settings page show when
//! the token last worked and why it stopped. An outage is not the token's
//! fault, so it is not recorded against it.

use std::fmt::{self, Debug, Formatter};
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, RETRY_AFTER, USER_AGENT};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;
use zeroize::Zeroizing;

use super::{FiledIssue, NewIssue, Tracker, TrackerError, TrackerFuture, clip, redact};

/// Where the REST API lives.
pub const API_ROOT: &str = "https://api.github.com";

/// The API version every request pins.
///
/// GitHub versions its REST API by date and answers an unpinned request with
/// whatever its default is that day.
const API_VERSION: &str = "2022-11-28";

/// How long one call may take.
///
/// Filing runs in a background job, so this bounds how long a worker slot is
/// held rather than how long anybody waits. GitHub answers in well under a
/// second when it is healthy.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Longest piece of GitHub's own message quoted in a refusal.
const GITHUB_MESSAGE_LIMIT: usize = 300;

/// What a repository name has to look like: `owner/name`.
static REPOSITORY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_.\-]+/[A-Za-z0-9_.\-]+$").expect("the repository pattern is valid")
});

/// Where to file, and the token to file with.
///
/// `Debug` shows the repository and nothing of the token, and the token is
/// cleared from memory when the target is dropped.
pub struct Target {
    repository: String,
    token: Zeroizing<String>,
}

impl Target {
    /// A target, once `repository` is a plain `owner/name`.
    ///
    /// # Errors
    /// Returns [`InvalidRepository`] when it is not: the name is spliced into
    /// request paths, so anything else could reach a different endpoint.
    pub fn new(repository: &str, token: &str) -> Result<Self, InvalidRepository> {
        if !is_repository(repository) {
            return Err(InvalidRepository);
        }

        Ok(Self {
            repository: repository.to_owned(),
            token: Zeroizing::new(token.to_owned()),
        })
    }

    /// The `owner/name` issues are filed in.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }
}

impl Debug for Target {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Target")
            .field("repository", &self.repository)
            .finish_non_exhaustive()
    }
}

/// Whether `repository` is a plain `owner/name`, which is all a target takes.
///
/// For a settings form validating what it was given before sealing anything.
#[must_use]
pub fn is_repository(repository: &str) -> bool {
    REPOSITORY.is_match(repository)
        && repository
            .split('/')
            .all(|segment| segment != "." && segment != "..")
}

/// A repository name that is not `owner/name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidRepository;

impl fmt::Display for InvalidRepository {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("a repository is named as owner/name")
    }
}

impl std::error::Error for InvalidRepository {}

/// How the token fared on its last call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage<'a> {
    /// GitHub accepted it.
    Accepted,
    /// GitHub refused it, and this says why in words an operator can act on.
    Refused(&'a str),
}

/// What a [`Credentials`] call returns.
pub type CredentialsFuture<'a, Value> = Pin<Box<dyn Future<Output = Value> + Send + 'a>>;

/// Where [`GitHub`] gets its token, and tells how it fared.
pub trait Credentials: Debug + Send + Sync + 'static {
    /// The target to file with right now, or `None` while none is configured.
    ///
    /// # Errors
    /// A [`TrackerError`] when the target exists and cannot be read, such as a
    /// sealed token the current key cannot open. Choose the kind by whether
    /// waiting helps: a database outage is unavailable, an unopenable token
    /// is refused.
    fn target(&self) -> CredentialsFuture<'_, Result<Option<Target>, TrackerError>>;

    /// Records how the token fared. Must not fail the call it describes.
    fn record<'a>(&'a self, usage: Usage<'a>) -> CredentialsFuture<'a, ()>;
}

/// Credentials configured once, at boot.
#[derive(Debug)]
pub struct Fixed(Target);

impl Fixed {
    /// Files every issue to `target`.
    #[must_use]
    pub fn new(target: Target) -> Self {
        Self(target)
    }
}

impl Credentials for Fixed {
    fn target(&self) -> CredentialsFuture<'_, Result<Option<Target>, TrackerError>> {
        Box::pin(async move {
            Ok(Some(Target {
                repository: self.0.repository.clone(),
                token: self.0.token.clone(),
            }))
        })
    }

    fn record<'a>(&'a self, _usage: Usage<'a>) -> CredentialsFuture<'a, ()> {
        // Nowhere to record it: a token configured at boot has no screen.
        Box::pin(async {})
    }
}

/// GitHub issues, as a [`Tracker`].
#[derive(Clone)]
pub struct GitHub {
    client: reqwest::Client,
    api_root: Arc<str>,
    credentials: Arc<dyn Credentials>,
}

impl Debug for GitHub {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHub")
            .field("api_root", &self.api_root)
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

impl GitHub {
    /// Files to github.com with whatever `credentials` hands out.
    #[must_use]
    pub fn new(credentials: Arc<dyn Credentials>) -> Self {
        Self::with_api_root(credentials, API_ROOT)
    }

    /// Files to the API at `api_root` instead, such as GitHub Enterprise or a
    /// test's fake.
    ///
    /// # Panics
    /// Panics when the HTTP client cannot be built, which means the TLS
    /// backend could not initialize: a broken build, not a runtime condition.
    #[must_use]
    pub fn with_api_root(credentials: Arc<dyn Credentials>, api_root: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("the GitHub client must build");

        Self {
            client,
            api_root: Arc::from(api_root.trim_end_matches('/')),
            credentials,
        }
    }

    /// Sends one request to the repository's API and decodes the answer.
    ///
    /// `path` is relative to the repository, such as `/issues`, and `query` is
    /// for the search endpoint, which is not.
    async fn call<Answer: DeserializeOwned>(
        &self,
        method: Method,
        endpoint: Endpoint<'_>,
        body: Option<serde_json::Value>,
    ) -> Result<Answer, TrackerError> {
        let target = self
            .credentials
            .target()
            .await?
            .ok_or_else(TrackerError::not_configured)?;

        let url = endpoint.url(&self.api_root, &target.repository)?;
        let mut request = self
            .client
            .request(method, url)
            .bearer_auth(target.token.as_str())
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            // GitHub refuses a request with no user agent.
            .header(USER_AGENT, concat!("anubis/", env!("CARGO_PKG_VERSION")));
        if let Some(body) = body {
            request = request
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string());
        }

        let response = request.send().await.map_err(|error| {
            TrackerError::unavailable(format!(
                "GitHub could not be reached: {}",
                redact(&error.without_url().to_string())
            ))
        })?;

        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.map_err(|error| {
            TrackerError::unavailable(format!(
                "GitHub's answer could not be read: {}",
                error.without_url()
            ))
        })?;

        if !status.is_success() {
            let error = refusal(status, &headers, &bytes, &target.repository);
            if let Some(reason) = error.is_refused().then(|| error.message()) {
                self.credentials.record(Usage::Refused(reason)).await;
            }
            return Err(error);
        }

        self.credentials.record(Usage::Accepted).await;
        serde_json::from_slice(&bytes).map_err(|error| {
            TrackerError::unavailable(format!("GitHub answered with unreadable JSON: {error}"))
        })
    }
}

/// Which endpoint a call is for.
enum Endpoint<'a> {
    /// A path under `/repos/{owner}/{name}`.
    Repository(&'a str),
    /// The issue search, with this query.
    Search(&'a str),
}

impl Endpoint<'_> {
    fn url(&self, api_root: &str, repository: &str) -> Result<Url, TrackerError> {
        let unparseable = |error: url::ParseError| {
            TrackerError::refused(format!("The GitHub API address is not a URL: {error}"))
        };

        match self {
            Self::Repository(path) => {
                Url::parse(&format!("{api_root}/repos/{repository}{path}")).map_err(unparseable)
            }
            Self::Search(query) => {
                let mut url =
                    Url::parse(&format!("{api_root}/search/issues")).map_err(unparseable)?;
                url.query_pairs_mut().append_pair("q", query);
                Ok(url)
            }
        }
    }
}

/// Turns a non-2xx answer into a refusal or an outage, in an operator's words.
///
/// **A `403` is two different things.** GitHub answers it both for a token that
/// lacks a permission and for a token that has spent its rate limit, and only
/// the headers tell them apart. The second is an outage that passes; the first
/// is a refusal that waits for a person.
fn refusal(status: StatusCode, headers: &HeaderMap, body: &[u8], repository: &str) -> TrackerError {
    #[derive(Deserialize)]
    struct Message {
        message: String,
    }

    let said = serde_json::from_slice::<Message>(body)
        .map(|parsed| clip(&redact(&parsed.message), GITHUB_MESSAGE_LIMIT))
        .unwrap_or_default();
    let quoted = if said.is_empty() {
        String::new()
    } else {
        format!(" GitHub said: \"{said}\"")
    };

    let rate_limited = headers.contains_key(RETRY_AFTER)
        || headers
            .get("x-ratelimit-remaining")
            .is_some_and(|remaining| remaining == "0");

    match status {
        StatusCode::UNAUTHORIZED => TrackerError::refused(format!(
            "GitHub refused the token (401). It has expired, been revoked, or was pasted \
             incorrectly.{quoted}"
        )),
        StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS if rate_limited => {
            TrackerError::unavailable(format!(
                "GitHub's rate limit is spent for now ({}).{quoted}",
                status.as_u16()
            ))
        }
        StatusCode::TOO_MANY_REQUESTS => {
            TrackerError::unavailable(format!("GitHub asked for fewer requests (429).{quoted}"))
        }
        StatusCode::FORBIDDEN => TrackerError::refused(format!(
            "GitHub refused the request (403). The token needs Issues: read and write on \
             {repository}, and an organization may still have to approve it.{quoted}"
        )),
        StatusCode::NOT_FOUND => TrackerError::refused(format!(
            "GitHub could not find {repository} (404). Check the name, and that the token was \
             granted access to it.{quoted}"
        )),
        StatusCode::GONE => TrackerError::refused(format!(
            "Issues are turned off on {repository} (410).{quoted}"
        )),
        status if status.is_server_error() => TrackerError::unavailable(format!(
            "GitHub is having trouble ({}).{quoted}",
            status.as_u16()
        )),
        status => TrackerError::refused(format!(
            "GitHub refused the request ({}).{quoted}",
            status.as_u16()
        )),
    }
}

/// The search that finds an abandoned filing's issue.
///
/// Every qualifier is load bearing: `is:open`, or a closed issue is adopted
/// over the rule that a recurrence after a fix is filed fresh; `in:body`, or a
/// fingerprint pasted into a comment makes an unrelated issue adoptable; and
/// the quotes, or GitHub may split the fingerprint into terms.
fn fingerprint_search(repository: &str, fingerprint: &str) -> String {
    format!("repo:{repository} is:issue is:open in:body \"{fingerprint}\"")
}

/// An issue as GitHub describes one.
#[derive(Deserialize)]
struct IssueAnswer {
    number: i64,
    html_url: String,
    #[serde(default)]
    state: String,
}

impl IssueAnswer {
    fn filed(self) -> FiledIssue {
        FiledIssue {
            number: self.number,
            url: self.html_url,
        }
    }
}

impl Tracker for GitHub {
    fn create<'a>(&'a self, issue: &'a NewIssue) -> TrackerFuture<'a, FiledIssue> {
        Box::pin(async move {
            let body = serde_json::json!({
                "title": issue.title,
                "body": issue.body,
                "labels": issue.labels,
            });
            let created: IssueAnswer = self
                .call(Method::POST, Endpoint::Repository("/issues"), Some(body))
                .await?;
            Ok(created.filed())
        })
    }

    fn find_open<'a>(&'a self, fingerprint: &'a str) -> TrackerFuture<'a, Option<FiledIssue>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Matches {
                items: Vec<IssueAnswer>,
            }

            let target = self
                .credentials
                .target()
                .await?
                .ok_or_else(TrackerError::not_configured)?;
            let query = fingerprint_search(target.repository(), fingerprint);
            drop(target);

            let matches: Matches = self
                .call(Method::GET, Endpoint::Search(&query), None)
                .await?;
            Ok(matches.items.into_iter().next().map(IssueAnswer::filed))
        })
    }

    fn is_open(&self, number: i64) -> TrackerFuture<'_, bool> {
        Box::pin(async move {
            let path = format!("/issues/{number}");
            let issue: IssueAnswer = self
                .call(Method::GET, Endpoint::Repository(&path), None)
                .await?;
            Ok(issue.state == "open")
        })
    }

    fn comment<'a>(&'a self, number: i64, body: &'a str) -> TrackerFuture<'a, ()> {
        Box::pin(async move {
            let path = format!("/issues/{number}/comments");
            let _created: serde_json::Value = self
                .call(
                    Method::POST,
                    Endpoint::Repository(&path),
                    Some(serde_json::json!({ "body": body })),
                )
                .await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

    use super::{Target, fingerprint_search, is_repository, refusal};

    #[test]
    fn a_target_never_prints_its_token() {
        let target = Target::new("mooreslabaiv1/benchmark", "github_pat_11ABCDEFGsecret")
            .expect("a plain owner/name is a repository");

        let rendered = format!("{target:?}");
        assert!(rendered.contains("mooreslabaiv1/benchmark"));
        assert!(!rendered.contains("github_pat"), "{rendered}");
    }

    #[test]
    fn only_a_plain_owner_and_name_is_a_repository() {
        assert!(is_repository("mooreslabaiv1/benchmark"));
        assert!(is_repository("a-b_c.d/e.f-g_h"));
        for bad in [
            "benchmark",
            "a/b/c",
            "../issues",
            "owner/..",
            "owner/name?x=1",
            "owner/name#1",
            " owner/name",
            "",
        ] {
            assert!(!is_repository(bad), "{bad:?} must be refused");
            Target::new(bad, "token").expect_err("a target must refuse it too");
        }
    }

    #[test]
    fn the_recovery_search_is_open_issues_bodies_and_the_exact_fingerprint() {
        assert_eq!(
            fingerprint_search("mooreslabaiv1/benchmark", "0123456789abcdef"),
            "repo:mooreslabaiv1/benchmark is:issue is:open in:body \"0123456789abcdef\""
        );
    }

    #[test]
    fn an_expired_token_is_a_refusal_that_says_so() {
        let error = refusal(
            StatusCode::UNAUTHORIZED,
            &HeaderMap::new(),
            br#"{"message":"Bad credentials"}"#,
            "o/r",
        );

        assert!(error.is_refused());
        assert!(error.message().contains("expired"));
        assert!(error.message().contains("Bad credentials"));
    }

    #[test]
    fn a_403_is_a_refusal_unless_the_rate_limit_is_spent() {
        let missing_permission = refusal(StatusCode::FORBIDDEN, &HeaderMap::new(), b"", "o/r");
        assert!(missing_permission.is_refused());
        assert!(
            missing_permission
                .message()
                .contains("Issues: read and write")
        );

        let mut spent = HeaderMap::new();
        spent.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        assert!(refusal(StatusCode::FORBIDDEN, &spent, b"", "o/r").is_unavailable());

        let mut wait = HeaderMap::new();
        wait.insert(RETRY_AFTER, HeaderValue::from_static("60"));
        assert!(refusal(StatusCode::FORBIDDEN, &wait, b"", "o/r").is_unavailable());
    }

    #[test]
    fn an_outage_is_retried_and_a_missing_repository_is_not() {
        assert!(refusal(StatusCode::BAD_GATEWAY, &HeaderMap::new(), b"", "o/r").is_unavailable());
        assert!(
            refusal(StatusCode::TOO_MANY_REQUESTS, &HeaderMap::new(), b"", "o/r").is_unavailable()
        );
        assert!(refusal(StatusCode::NOT_FOUND, &HeaderMap::new(), b"", "o/r").is_refused());
        assert!(refusal(StatusCode::GONE, &HeaderMap::new(), b"", "o/r").is_refused());
    }

    #[test]
    fn a_quoted_message_is_redacted_before_it_is_stored() {
        let error = refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            &HeaderMap::new(),
            br#"{"message":"token ghp_0123456789abcdefghijABCDEFGHIJ is invalid"}"#,
            "o/r",
        );

        assert!(!error.message().contains("ghp_"), "{}", error.message());
    }
}
