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
use serde::Deserialize;
use std::collections::HashSet;

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
    let mut req = http
        .get(url)
        .header("accept", "application/vnd.github+json")
        .header("user-agent", "tool-artifact-registry");
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
    let repo = repo.trim().trim_start_matches("https://github.com/").trim_end_matches('/');
    if repo.split('/').count() != 2 {
        return Err(format!("{repo:?} is not an owner/name repository"));
    }

    let Some(meta) = get::<Repo>(http, &format!("{API}/repos/{repo}"), token).await? else {
        return Err(format!("no repository {repo}, or it is private and the credential cannot see it"));
    };
    let branch = meta.default_branch.clone().unwrap_or_else(|| "main".into());

    let mut set = |name: &str, current: &mut Option<String>, next: Option<String>, out: &mut SyncOutcome| {
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
                        let base = (!meta.private)
                            .then(|| format!("https://raw.githubusercontent.com/{repo}/{branch}/"));
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
        if let Some(releases) = get::<Vec<GhRelease>>(http, &format!("{API}/repos/{repo}/releases?per_page=20"), token).await? {
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
