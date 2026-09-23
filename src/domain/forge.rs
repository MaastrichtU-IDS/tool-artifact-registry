//! Keeping a Software record in step with its source repository.
//!
//! ## The rule that makes this safe
//!
//! Sync overwrites **only the fields the record named as managed**. Everything else belongs to
//! whoever curated it and is left alone, even when the repository has an obvious value for it.
//!
//! That constraint is the whole design. A sync that "helpfully" refreshed everything would
//! silently discard the sentence a curator wrote because it was better than the repo's
//! one-liner, and the loss would be invisible until someone noticed the page had got worse.
//! Naming the managed fields makes the trade explicit at the moment somebody opts in, and the
//! record reports what the last run changed so a surprise is at least auditable.
//!
//! ## Credentials
//!
//! Public repositories need none. A private one needs a token, and there are two ways to get
//! one, in order of preference:
//!
//! 1. the signed-in curator's own GitHub token, brokered by Keycloak — then the registry can
//!    read exactly what that person can read, and nothing more;
//! 2. `TAR_FORGE_TOKEN`, a registry-wide token (spec §10.5). Simpler, but it means every
//!    curator can pull anything that token can see.

use crate::error::{AppError, AppResult};
use crate::model::{ReleaseIn, SoftwareIn, SYNCABLE_FIELDS};
use crate::ns;
use crate::state::AppState;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

const API: &str = "https://api.github.com";

#[derive(Debug, Deserialize)]
struct Repo {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    topics: Vec<String>,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    default_branch: Option<String>,
    #[serde(default)]
    license: Option<RepoLicense>,
    #[serde(default)]
    private: bool,
    // The liveness numbers. Options rather than defaulted counts, so a response that lacks
    // one stores "unknown" instead of a zero nobody measured.
    #[serde(default)]
    stargazers_count: Option<i64>,
    #[serde(default)]
    forks_count: Option<i64>,
    #[serde(default)]
    pushed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RepoLicense {
    #[serde(default)]
    spdx_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Readme {
    #[serde(default)]
    download_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    #[serde(default)]
    tag_name: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: Option<i64>,
}

/// The credential to read a repository with.
///
/// Preference order matters. A curator's own brokered GitHub token reads exactly what that
/// person can read and nothing more, which is the property worth having: a shared registry
/// token means anyone who can press "sync" can pull anything that token can see, including
/// private repositories they have no business reading.
///
/// The brokered path needs Keycloak's token-exchange endpoint and is wired separately; until
/// then this falls back to the registry-wide token from spec §10.5.
pub fn token_for(brokered: Option<String>) -> Option<String> {
    brokered.or_else(|| {
        // Read directly rather than through Config: `src/config.rs` is being edited elsewhere,
        // and this belongs there once that settles.
        std::env::var("TAR_FORGE_TOKEN").ok().filter(|t| !t.trim().is_empty())
    })
}

/// What a sync run did, so the caller can show it rather than assert that something happened.
#[derive(Debug, Default)]
pub struct SyncOutcome {
    pub changed: Vec<String>,
    pub releases: Vec<ReleaseIn>,
    pub skipped: Vec<String>,
}

fn managed(fields: &[String], name: &str) -> bool {
    fields.iter().any(|f| f == name)
}

/// Validate the field list at the point somebody sets it, not at the point sync runs.
pub fn check_fields(fields: &[String]) -> AppResult<()> {
    let unknown: Vec<&String> = fields.iter().filter(|f| !SYNCABLE_FIELDS.contains(&f.as_str())).collect();
    if !unknown.is_empty() {
        return Err(AppError::bad_request(format!(
            "cannot sync {} from a repository; syncable fields are {}",
            unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            SYNCABLE_FIELDS.join(", ")
        )));
    }
    Ok(())
}

async fn get<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> Result<Option<T>, String> {
    let mut req =
        http.get(url).header("accept", "application/vnd.github+json").header("user-agent", "tool-artifact-registry");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let resp = req.send().await.map_err(|e| format!("{url}: {e}"))?;
    match resp.status().as_u16() {
        200 => resp.json::<T>().await.map(Some).map_err(|e| format!("{url}: unreadable response: {e}")),
        // A repo with no releases is not an error, and neither is a README that does not exist.
        404 => Ok(None),
        401 | 403 => Err(format!(
            "{url}: GitHub refused the request ({}). A private repository needs a token — sign in \
             with GitHub, or set TAR_FORGE_TOKEN.",
            resp.status()
        )),
        s => Err(format!("{url}: GitHub returned {s}")),
    }
}

/// Fetch the managed fields and apply them to `input`, leaving everything else untouched.
pub async fn sync_into(
    http: &reqwest::Client,
    repo: &str,
    fields: &[String],
    token: Option<&str>,
    input: &mut SoftwareIn,
) -> Result<SyncOutcome, String> {
    let mut out = SyncOutcome::default();
    let Some(repo) = github_repo(repo) else {
        return Err(format!("{repo:?} is not an owner/name repository"));
    };

    let Some(meta) = get::<Repo>(http, &format!("{API}/repos/{repo}"), token).await? else {
        return Err(format!("no repository {repo}, or it is private and the credential cannot see it"));
    };
    let branch = meta.default_branch.clone().unwrap_or_else(|| "main".into());

    let set = |name: &str, current: &mut Option<String>, next: Option<String>, out: &mut SyncOutcome| {
        let next = next.filter(|v| !v.trim().is_empty());
        if next.is_some() && next.as_deref() != current.as_deref() {
            *current = next;
            out.changed.push(name.to_string());
        }
    };

    if managed(fields, "tagline") {
        set("tagline", &mut input.tagline, meta.description.clone(), &mut out);
    }
    if managed(fields, "homepage") {
        set("homepage", &mut input.homepage, meta.homepage.clone().filter(|h| h.starts_with("http")), &mut out);
    }
    if managed(fields, "license") {
        // GitHub reports NOASSERTION when it sees a licence file it cannot identify; that is
        // not an SPDX id and must not become one.
        let spdx = meta
            .license
            .as_ref()
            .and_then(|l| l.spdx_id.clone())
            .filter(|s| !s.is_empty() && s != "NOASSERTION")
            .map(|s| format!("https://spdx.org/licenses/{s}"));
        if spdx.is_none() {
            out.skipped.push("license (the repository declares none)".into());
        }
        set("license", &mut input.license, spdx, &mut out);
    }
    if managed(fields, "keywords") && !meta.topics.is_empty() {
        let mut merged: Vec<String> = meta.topics.clone();
        // Keep anything a curator added that GitHub does not know about.
        for k in &input.keywords {
            if !merged.iter().any(|m| m.eq_ignore_ascii_case(k)) {
                merged.push(k.clone());
            }
        }
        if merged != input.keywords {
            input.keywords = merged;
            out.changed.push("keywords".into());
        }
    }
    if managed(fields, "maturity") && meta.archived {
        // The one status GitHub actually knows. It says nothing about a live repo's maturity,
        // so an unarchived repo leaves the curator's value alone rather than guessing "active".
        set("maturity", &mut input.maturity, Some("inactive".into()), &mut out);
    }
    if managed(fields, "readme") {
        match get::<Readme>(http, &format!("{API}/repos/{repo}/readme"), token).await? {
            // Ask GitHub where the README is rather than guessing: repositories spell it
            // README.md, Readme.md and README.MD, and a guess silently 404s.
            Some(Readme { download_url: Some(url) }) => {
                let mut req = http.get(&url).header("user-agent", "tool-artifact-registry");
                if let Some(t) = token {
                    req = req.header("authorization", format!("Bearer {t}"));
                }
                match req.send().await {
                    Ok(r) if r.status().is_success() => {
                        let body = r.text().await.unwrap_or_default();
                        if !body.is_empty() && input.readme.as_deref() != Some(body.as_str()) {
                            input.readme = Some(body);
                            out.changed.push("readme".into());
                        }
                        // Relative images in a README only resolve against the raw root, and a
                        // private repository has no anonymous one — so do not set a base that
                        // would render every image broken for readers.
                        let base =
                            (!meta.private).then(|| format!("https://raw.githubusercontent.com/{repo}/{branch}/"));
                        if base.is_some() && input.readme_base_url != base {
                            input.readme_base_url = base;
                            out.changed.push("readme_base_url".into());
                        } else if meta.private {
                            out.skipped.push("readme_base_url (private repository has no public raw root)".into());
                        }
                    }
                    _ => out.skipped.push("readme (could not download it)".into()),
                }
            }
            _ => out.skipped.push("readme (the repository has none)".into()),
        }
    }
    if managed(fields, "releases") {
        if let Some(releases) =
            get::<Vec<GhRelease>>(http, &format!("{API}/repos/{repo}/releases?per_page=20"), token).await?
        {
            let mut seen: HashSet<String> = HashSet::new();
            for r in releases.into_iter().filter(|r| !r.draft) {
                let Some(tag) = r.tag_name.clone().filter(|t| !t.is_empty()) else { continue };
                let version = tag.trim_start_matches('v').to_string();
                if !seen.insert(version.clone()) {
                    continue;
                }
                out.releases.push(ReleaseIn {
                    version,
                    date_published: r.published_at.clone(),
                    changelog: r.html_url.clone(),
                    downloads: r
                        .assets
                        .iter()
                        .map(|a| crate::model::DownloadIn {
                            url: a.browser_download_url.clone(),
                            label: Some(a.name.clone()),
                            platform: platform_of(&a.name),
                            byte_size: a.size,
                            availability: Some("public".into()),
                        })
                        .collect(),
                    ..Default::default()
                });
                if r.prerelease {
                    out.skipped.push(format!("{tag} is a pre-release"));
                }
            }
            // Deliberately not marked changed here: fetching releases is not the same as
            // adding one. Only the caller knows which versions were actually new, and a change
            // log that reports non-changes is worse than no change log.
        }
    }
    Ok(out)
}

/// Guess the platform from an asset filename. Wrong guesses are better than no label here,
/// because the filename is shown next to it either way.
fn platform_of(name: &str) -> Option<String> {
    let n = name.to_ascii_lowercase();
    let p = if n.ends_with(".exe") || n.ends_with(".msi") || n.contains("windows") || n.contains("win64") {
        "Windows"
    } else if n.ends_with(".dmg") || n.contains("mac") || n.contains("darwin") || n.contains("osx") {
        "macOS"
    } else if n.ends_with(".appimage") || n.ends_with(".deb") || n.ends_with(".rpm") || n.contains("linux") {
        "Linux"
    } else {
        return None;
    };
    Some(p.to_string())
}

/// The `owner/name` a repository string names on GitHub, or `None` if it names none.
///
/// Accepts what curators actually paste: a bare `owner/name`, a repository URL, a URL deeper
/// into the repository, or one ending in `.git`. Shared by sync and the poller so the two can
/// never disagree about which repository a record has.
pub fn github_repo(s: &str) -> Option<String> {
    let s = s.trim();
    let path = ["https://github.com/", "http://github.com/", "https://www.github.com/"]
        .iter()
        .find_map(|p| s.strip_prefix(p))
        .or_else(|| (!s.contains("://")).then_some(s))?;
    let path = path.split(['?', '#']).next()?;
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    let owner = parts.next()?;
    let name = parts.next()?.trim_end_matches(".git");
    // A GitHub owner cannot contain a dot, so a bare `gitlab.com/a/b` is not misread as one.
    if owner.contains('.') || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// A record's GitHub repository: the one it syncs from, else its `code_repository`.
pub fn repo_of(sync_repo: Option<&str>, code_repository: Option<&str>) -> Option<String> {
    sync_repo.and_then(github_repo).or_else(|| code_repository.and_then(github_repo))
}

// ----------------------------------------------------------------- liveness poller
//
// Stars, forks and last push for the signal bar (design note: repository liveness). Kept in
// SQLite, not the graph: they are a cached copy of GitHub's numbers, not a claim this registry
// makes, so neither `/sparql` nor a peer sees them.

/// What one poll learns about a repository.
#[derive(Debug, Clone, Default)]
pub struct RepoStats {
    pub stars: Option<i64>,
    pub forks: Option<i64>,
    pub pushed_at: Option<String>,
}

/// Fetch a repository's liveness numbers. One request, and the same one sync makes first.
pub async fn repo_stats(http: &reqwest::Client, repo: &str, token: Option<&str>) -> Result<RepoStats, String> {
    let meta = get::<Repo>(http, &format!("{API}/repos/{repo}"), token)
        .await?
        .ok_or_else(|| format!("no repository {repo}, or it is private and the credential cannot see it"))?;
    Ok(RepoStats { stars: meta.stargazers_count, forks: meta.forks_count, pushed_at: meta.pushed_at })
}

/// The gap between two requests: the pass spread evenly over the interval, but never closer
/// together than `floor`, the pace the credential's rate limit allows.
pub fn pace(interval: Duration, due: usize, floor: Duration) -> Duration {
    match u32::try_from(due) {
        Ok(n) if n > 0 => (interval / n).max(floor),
        _ => floor,
    }
}

/// Local, non-withdrawn software with a GitHub repository, as `(iri, owner/name)`.
///
/// Local only: a peer's records are the peer's to poll, and spending our rate budget on them
/// would buy numbers we are not going to publish anyway.
fn targets(state: &AppState) -> Vec<(String, String)> {
    let q = format!(
        r#"{p}
SELECT ?s ?code ?sync WHERE {{
  GRAPH <{g}> {{
    ?s a <{t}> .
    OPTIONAL {{ ?s schema:codeRepository ?code }}
    OPTIONAL {{ ?s tar:sync ?n . ?n tar:syncRepo ?sync }}
  }}
  FILTER NOT EXISTS {{ GRAPH ?tg {{ ?s tar:tombstoned true }} }}
}}"#,
        p = ns::PREFIXES,
        g = ns::G_LOCAL,
        t = crate::domain::software::TYPE_SOFTWARE,
    );
    let rows = match state.store.select(&q) {
        Ok(b) => b.rows,
        Err(e) => {
            tracing::warn!(error = %e, "could not list software for the forge poller");
            return Vec::new();
        }
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for r in &rows {
        let (Some(iri), Some(repo)) = (r.iri("s"), repo_of(r.str("sync").as_deref(), r.iri("code").as_deref())) else {
            continue;
        };
        if !out.iter().any(|(i, _)| *i == iri) {
            out.push((iri, repo));
        }
    }
    out
}

/// Refresh every record that is due: never polled, polled for a different repository, or
/// last tried more than `interval` ago. Returns how many were fetched.
///
/// `fetch` is the forge call, passed in so the tests can stand in for GitHub without a
/// network; the loop passes [`repo_stats`].
pub async fn poll_once<F, Fut>(state: &AppState, interval: Duration, floor: Duration, fetch: F) -> usize
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<RepoStats, String>>,
{
    let cutoff = chrono::Utc::now() - chrono::Duration::from_std(interval).unwrap_or(chrono::Duration::MAX);
    let mut due = Vec::new();
    for (iri, repo) in targets(state) {
        let last = state.ops.repository_stats(&iri).await.ok().flatten();
        let fresh = last.is_some_and(|l| {
            l.repo == repo && chrono::DateTime::parse_from_rfc3339(&l.checked_at).is_ok_and(|t| t > cutoff)
        });
        if !fresh {
            due.push((iri, repo));
        }
    }
    let gap = pace(interval, due.len(), floor);
    for (i, (iri, repo)) in due.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(gap).await;
        }
        let written = match fetch(repo.clone()).await {
            Ok(s) => state.ops.record_repository_stats(iri, repo, s.stars, s.forks, s.pushed_at.as_deref()).await,
            Err(e) => {
                tracing::debug!(software = %iri, %repo, error = %e, "forge poll failed");
                state.ops.record_repository_error(iri, repo, &e).await
            }
        };
        if let Err(e) = written {
            tracing::warn!(software = %iri, error = %e, "could not record repository stats");
        }
    }
    due.len()
}

/// The background loop, spawned beside the health prober.
///
/// Same client, credential and egress as sync: `AppState::http` (so the process's proxy
/// settings apply) and `token_for`, which is `TAR_FORGE_TOKEN` when set. The poller acts for
/// nobody, so there is no brokered token to prefer.
pub async fn poll_loop(state: Arc<AppState>) {
    let interval = state.config.forge_poll_interval;
    let token = token_for(None);
    // GitHub allows 5,000 requests an hour with a token and 60 without. The floors spend at
    // most 1,800 and 30 of those, leaving the rest for curators pressing "sync", which shares
    // the budget and costs up to four requests a press.
    let floor = Duration::from_secs(if token.is_some() { 2 } else { 120 });
    tracing::info!(?interval, authenticated = token.is_some(), "polling GitHub for repository stats");
    loop {
        let (http, token) = (state.http.clone(), token.clone());
        poll_once(&state, interval, floor, move |repo| {
            let (http, token) = (http.clone(), token.clone());
            async move { repo_stats(&http, &repo, token.as_deref()).await }
        })
        .await;
        // Between passes, wake at least hourly: `poll_once` skips everything not yet due, so
        // waking often costs nothing. A pass itself is spread over the interval, though, so a
        // record created while one is running waits for the next pass — up to about one
        // interval. The lower bound stops a zero interval from spinning on the store.
        tokio::time::sleep(interval.clamp(Duration::from_secs(60), Duration::from_secs(3600))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_repositories_are_recognised_however_they_are_written() {
        for s in [
            "MaastrichtU-IDS/shacl-manager",
            "https://github.com/MaastrichtU-IDS/shacl-manager",
            "https://github.com/MaastrichtU-IDS/shacl-manager/",
            "https://github.com/MaastrichtU-IDS/shacl-manager.git",
            "http://github.com/MaastrichtU-IDS/shacl-manager/tree/main",
            "https://www.github.com/MaastrichtU-IDS/shacl-manager#readme",
        ] {
            assert_eq!(github_repo(s).as_deref(), Some("MaastrichtU-IDS/shacl-manager"), "{s}");
        }
        for s in ["https://gitlab.com/a/b", "gitlab.com/a/b", "https://github.com/only-owner", "", "git@github.com:a/b"]
        {
            assert_eq!(github_repo(s), None, "{s}");
        }
        // The sync repository wins over the code link, which may point anywhere.
        assert_eq!(repo_of(Some("a/b"), Some("https://github.com/c/d")).as_deref(), Some("a/b"));
        assert_eq!(repo_of(None, Some("https://codeberg.org/c/d")), None);
    }

    #[test]
    fn requests_are_spread_over_the_interval_but_never_faster_than_the_floor() {
        let day = Duration::from_secs(86400);
        let floor = Duration::from_secs(120);
        assert_eq!(pace(day, 10, floor), Duration::from_secs(8640));
        // More records than the floor fits into a day: the floor wins and the pass runs long.
        assert_eq!(pace(day, 5000, floor), floor);
        assert_eq!(pace(day, 0, floor), floor);
    }
}
