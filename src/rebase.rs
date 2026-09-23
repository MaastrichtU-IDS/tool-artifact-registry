//! Changing `TAR_BASE_IRI` after data exists (design note
//! `docs/specs/2026-09-23-base-iri-rebase.md`, answering spec Q9).
//!
//! Two halves. `tar rebase --from <old>` renames this registry's own records, and every
//! reference to them in its own stores, from the old base to the current one. Everything else —
//! peers, exported files, CI pipelines — keeps the old IRIs forever, so with
//! `TAR_PREVIOUS_BASE_IRIS` set the running registry keeps answering for them: it redirects a
//! request addressed to an old host, and translates old IRIs in what a client sends.

use crate::state::AppState;
use crate::store::GraphTx;
use anyhow::{bail, Context, Result};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{GraphName, NamedNode, NamedOrBlankNode, Quad, Term};
use std::sync::Arc;

// ------------------------------------------------------------------------------- bases

/// Refuse a pair of bases that cannot be rebased cleanly. Nested bases are the dangerous case:
/// with `https://x.org` → `https://x.org/registry`, every new IRI still starts with the old
/// base, so a second run — which is otherwise always safe — would rename them again.
pub fn check_bases(old: &str, new: &str) -> Result<()> {
    let (old, new) = (old.trim_end_matches('/'), new.trim_end_matches('/'));
    if !(old.starts_with("http://") || old.starts_with("https://")) {
        bail!("{old:?} is not an http(s) URL");
    }
    if old == new {
        bail!("{old:?} is the current TAR_BASE_IRI");
    }
    if is_under(new, old) || is_under(old, new) {
        bail!("{old:?} and {new:?} are nested; one base cannot be a prefix of the other");
    }
    Ok(())
}

fn is_under(iri: &str, base: &str) -> bool {
    iri.strip_prefix(base).is_some_and(|rest| rest.starts_with('/'))
}

/// `<old>/x` → `<new>/x`, and `<old>` itself → `<new>`: the base is also the IRI of the
/// registry's own catalog, which every Instance is `dcat:inCatalog`. Only a whole `<old>/`
/// prefix matches, so `https://old.orgx/…` is somebody else's IRI and is left alone.
fn rewrite_iri(iri: &str, old: &str, new: &str) -> Option<String> {
    match iri.strip_prefix(old)? {
        "" => Some(new.to_string()),
        rest => rest.strip_prefix('/').map(|rest| format!("{new}/{rest}")),
    }
}

/// Every `<old>/` in a text body, JSON or SPARQL, becomes `<new>/`. The trailing slash is what
/// keeps `https://old.org.example/` from matching `https://old.org`.
fn rewrite_text(text: &str, old: &str, new: &str) -> String {
    text.replace(&format!("{old}/"), &format!("{new}/"))
}

/// The same, for `a=b&c=d`: each value decoded, rewritten and re-encoded, because an IRI in a
/// query string arrives as `https%3A%2F%2F…` and a plain replace would never see it.
fn rewrite_form(form: &str, olds: &[String], new: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(url::form_urlencoded::parse(form.as_bytes()).map(|(k, v)| {
            let v = olds.iter().fold(v.into_owned(), |v, old| rewrite_text(&v, old, new));
            (k.into_owned(), v)
        }))
        .finish()
}

// ------------------------------------------------------------------------------ command

/// What a rebase changed, or with `--dry-run`, would change.
#[derive(Debug, Default)]
pub struct Report {
    /// Statements in `<urn:tar:local>` with at least one renamed IRI.
    pub statements: usize,
    /// `(table.column, rows)`, only the columns that had any.
    pub rows: Vec<(String, u64)>,
    /// The local graph as it was before the rewrite, as N-Quads, when there is a data
    /// directory to keep it in.
    pub snapshot: Option<std::path::PathBuf>,
}

impl Report {
    pub fn is_empty(&self) -> bool {
        self.statements == 0 && self.rows.is_empty()
    }
}

/// How an ops column holds IRIs.
#[derive(Clone, Copy)]
enum Holds {
    /// The whole value is one IRI.
    Iri,
    /// JSON: renamed wherever a string starts with the old base.
    Json,
    /// IRIs joined with other text, as `run|artifact|role` in an idempotency key: renamed
    /// wherever `<old>/` occurs.
    Joined,
}

/// Columns in the ops database that hold this registry's own IRIs. Peers' bases, the resolve
/// queue and federated query ids are other registries' IRIs.
const OPS_COLUMNS: [(&str, &str, Holds); 14] = [
    ("api_tokens", "instance_iri", Holds::Iri),
    ("api_tokens", "software_iri", Holds::Iri),
    ("subscriptions", "instance_iri", Holds::Iri),
    ("subscriptions", "filter", Holds::Json),
    ("subscription_deliveries", "artifact_iri", Holds::Iri),
    ("subscription_deliveries", "run_iri", Holds::Iri),
    ("subscription_deliveries", "payload", Holds::Json),
    ("run_keys", "instance_iri", Holds::Iri),
    ("run_keys", "run_iri", Holds::Iri),
    ("artifact_keys", "artifact_iri", Holds::Iri),
    // The key is `run_iri|artifact_iri|role` (`Ops::claim_advertisement`); left stale, a
    // retried advertisement after the move would miss it and be applied twice.
    ("advertise_idem", "idem_key", Holds::Joined),
    ("advertise_idem", "run_iri", Holds::Iri),
    ("advertise_idem", "artifact_iri", Holds::Iri),
    ("audit_log", "target", Holds::Iri),
];

/// Rename everything under `old` to the current base. Graph first, then the ops database, each
/// in one transaction; each only touches values still under `old`, so a run interrupted between
/// the two is finished by running it again, and a run on a finished store changes nothing.
pub async fn run(state: &AppState, old: &str, dry_run: bool) -> Result<Report> {
    let new = state.config.base_iri.trim_end_matches('/');
    let old = old.trim_end_matches('/');
    check_bases(old, new)?;
    let mut report = Report::default();

    // The graph. Read whole, rewritten in memory, and written back by clearing the graph and
    // inserting the result in one transaction: a failure leaves it as it was.
    let local = NamedNode::new(crate::ns::G_LOCAL)?;
    let dump = state.store.dump_nquads(Some(crate::ns::G_LOCAL))?;
    let (mut before, mut quads) = (Vec::new(), Vec::new());
    for q in RdfParser::from_format(RdfFormat::NTriples).for_slice(dump.as_bytes()) {
        let q = q.context("reading the local graph")?;
        before.push(Quad::new(q.subject.clone(), q.predicate.clone(), q.object.clone(), local.clone()));
        let (rewritten, changed) = rewrite_quad(q, old, new, &local);
        report.statements += usize::from(changed);
        quads.push(rewritten);
    }
    if report.statements > 0 && !dry_run {
        // Kept before anything is written. An external endpoint may run the clear and the
        // insert of one request separately (limitations §16); if the insert then fails, this
        // file is the graph, and `tar restore` puts it back.
        report.snapshot = snapshot(&state.config.data_dir, &before)?;
        let mut tx = GraphTx::new();
        tx.clear_graphs.push(crate::ns::G_LOCAL.to_string());
        tx.extend(quads);
        state.store.apply(tx).with_context(|| match &report.snapshot {
            Some(p) => format!(
                "writing the rebased local graph; if it is now missing or partial, restore it with \
                 `tar restore --nquads {}` and run the rebase again",
                p.display()
            ),
            None => "writing the rebased local graph".into(),
        })?;
    }

    // The ops database.
    let (old_prefix, new_prefix) = (format!("{old}/"), format!("{new}/"));
    let (old_json, new_json) = (format!("\"{old}/"), format!("\"{new}/"));
    let mut db = state.ops.pool().begin().await?;
    for (table, column, holds) in OPS_COLUMNS {
        let (count, update) = if !matches!(holds, Holds::Iri) {
            (
                format!("SELECT COUNT(*) FROM {table} WHERE instr({column}, ?1) > 0"),
                format!("UPDATE {table} SET {column} = replace({column}, ?1, ?2) WHERE instr({column}, ?1) > 0"),
            )
        } else {
            // Compared by `substr`, not `LIKE`: `_` in a base would be a wildcard there.
            (
                format!("SELECT COUNT(*) FROM {table} WHERE substr({column}, 1, length(?1)) = ?1"),
                format!(
                    "UPDATE {table} SET {column} = ?2 || substr({column}, length(?1) + 1) \
                     WHERE substr({column}, 1, length(?1)) = ?1"
                ),
            )
        };
        let (from, to) = match holds {
            Holds::Json => (&old_json, &new_json),
            Holds::Iri | Holds::Joined => (&old_prefix, &new_prefix),
        };
        let n: i64 = sqlx::query_scalar(&count).bind(from).fetch_one(&mut *db).await?;
        if n == 0 {
            continue;
        }
        if !dry_run {
            sqlx::query(&update).bind(from).bind(to).execute(&mut *db).await?;
        }
        report.rows.push((format!("{table}.{column}"), n as u64));
    }
    if dry_run {
        db.rollback().await?;
    } else {
        db.commit().await?;
    }
    Ok(report)
}

/// Write `quads` to `{data_dir}/rebase-before-{time}.nq`. Nothing for an in-memory store, which
/// a failed write loses either way.
fn snapshot(data_dir: &str, quads: &[Quad]) -> Result<Option<std::path::PathBuf>> {
    if data_dir == "memory" {
        return Ok(None);
    }
    let path = std::path::Path::new(data_dir)
        .join(format!("rebase-before-{}.nq", chrono::Utc::now().format("%Y%m%dT%H%M%SZ")));
    let mut out = RdfSerializer::from_format(RdfFormat::NQuads).for_writer(Vec::new());
    for q in quads {
        out.serialize_quad(q)?;
    }
    std::fs::write(&path, out.finish()?).with_context(|| format!("writing {}", path.display()))?;
    Ok(Some(path))
}

fn rewrite_quad(q: Quad, old: &str, new: &str, graph: &NamedNode) -> (Quad, bool) {
    let mut changed = false;
    let mut named = |n: NamedNode| match rewrite_iri(n.as_str(), old, new) {
        Some(iri) => {
            changed = true;
            NamedNode::new_unchecked(iri)
        }
        None => n,
    };
    let subject = match q.subject {
        NamedOrBlankNode::NamedNode(n) => NamedOrBlankNode::NamedNode(named(n)),
        b => b,
    };
    let predicate = named(q.predicate);
    let object = match q.object {
        Term::NamedNode(n) => Term::NamedNode(named(n)),
        o => o,
    };
    (Quad::new(subject, predicate, object, GraphName::NamedNode(graph.clone())), changed)
}

/// A base other than the current one that this registry's own records are still under, if
/// there is one — someone changed `TAR_BASE_IRI` without running `tar rebase`. Software and
/// Instances only: those are always minted here, whereas the local graph legitimately holds
/// foreign subjects of other kinds (an adopted type, an organisation, a content hash).
pub fn stray_base(state: &AppState) -> Result<Option<String>> {
    let base = state.config.base_iri.trim_end_matches('/');
    let q = format!(
        "SELECT ?s WHERE {{ GRAPH <{}> {{ ?s ?p ?o }} \
         FILTER(isIRI(?s) && !STRSTARTS(STR(?s), \"{base}/\") \
         && REGEX(STR(?s), \"^https?://.+/(software|instance)/[0-9a-f]{{8}}-[0-9a-f-]{{27}}$\")) }} LIMIT 1",
        crate::ns::G_LOCAL
    );
    let found = state.store.select(&q)?.rows.first().and_then(|r| r.iri("s"));
    Ok(found.and_then(|s| {
        let (head, _id) = s.rsplit_once('/')?;
        Some(head.rsplit_once('/')?.0.to_string())
    }))
}

// --------------------------------------------------------------------------- middleware

/// With `TAR_PREVIOUS_BASE_IRIS` set: a request addressed to an old host is redirected to the
/// same path under the current base, and old IRIs in the query string or a textual body are
/// translated to current ones, so that what is stored has one spelling and no handler needs to
/// know the registry ever moved.
pub async fn middleware(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let previous = &state.config.previous_base_iris;
    if previous.is_empty() {
        return next.run(req).await;
    }
    let new = state.config.base_iri.trim_end_matches('/');
    if let Some(location) = redirect_target(&req, previous, new) {
        return match HeaderValue::from_str(&location) {
            Ok(v) => (StatusCode::PERMANENT_REDIRECT, [(header::LOCATION, v)]).into_response(),
            Err(_) => next.run(req).await,
        };
    }
    match translate(req, previous, new, state.config.max_payload_bytes).await {
        Ok(req) => next.run(req).await,
        Err(resp) => resp,
    }
}

/// Where a request to an old host belongs, if it is one. Compared on host, port and path
/// prefix; the scheme is not visible behind a TLS-terminating proxy, and does not matter.
fn redirect_target(req: &Request, previous: &[String], new: &str) -> Option<String> {
    let host = req.headers().get(header::HOST)?.to_str().ok()?;
    let path_and_query = req.uri().path_and_query().map_or("/", |p| p.as_str());
    previous.iter().find_map(|old| {
        let url = url::Url::parse(old).ok()?;
        let authority = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str()?),
            None => url.host_str()?.to_string(),
        };
        if !host.eq_ignore_ascii_case(&authority) {
            return None;
        }
        let prefix = url.path().trim_end_matches('/');
        let rest = path_and_query.strip_prefix(prefix)?;
        (rest.is_empty() || rest.starts_with(['/', '?'])).then(|| format!("{new}{rest}"))
    })
}

async fn translate(req: Request, previous: &[String], new: &str, limit: usize) -> Result<Request, Response> {
    let (mut parts, body) = req.into_parts();
    if let Some(q) = parts.uri.query().filter(|q| previous.iter().any(|old| mentions(q, old))) {
        let rewritten = format!("{}?{}", parts.uri.path(), rewrite_form(q, previous, new));
        if let Ok(uri) = rewritten.parse::<Uri>() {
            parts.uri = uri;
        }
    }
    let content_type = parts.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    let (is_form, is_text) = (
        content_type.starts_with("application/x-www-form-urlencoded"),
        content_type.contains("json") || content_type.starts_with("application/sparql"),
    );
    if !is_form && !is_text {
        // Artifact bytes and anything else opaque pass through untouched and unbuffered.
        return Ok(Request::from_parts(parts, body));
    }
    let bytes = axum::body::to_bytes(body, limit).await.map_err(|_| {
        crate::error::AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "payload-too-large", "Payload too large")
            .detail(format!("the body exceeds {limit} bytes"))
            .into_response()
    })?;
    let Ok(text) = std::str::from_utf8(&bytes) else { return Ok(Request::from_parts(parts, Body::from(bytes))) };
    if !previous.iter().any(|old| mentions(text, old)) {
        return Ok(Request::from_parts(parts, Body::from(bytes)));
    }
    let text = if is_form {
        rewrite_form(text, previous, new)
    } else {
        previous.iter().fold(text.to_string(), |t, old| rewrite_text(&t, old, new))
    };
    parts.headers.insert(header::CONTENT_LENGTH, HeaderValue::from(text.len()));
    Ok(Request::from_parts(parts, Body::from(text)))
}

/// Whether `text` could contain an IRI under `old`, raw or percent-encoded. The host is never
/// encoded, so looking for it finds both spellings.
fn mentions(text: &str, old: &str) -> bool {
    let host = old.split("://").nth(1).unwrap_or(old).split('/').next().unwrap_or(old);
    text.contains(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "https://old.example.org";
    const NEW: &str = "https://reg.example.org";

    #[test]
    fn only_a_whole_old_prefix_is_renamed() {
        assert_eq!(
            rewrite_iri(&format!("{OLD}/software/1"), OLD, NEW).as_deref(),
            Some("https://reg.example.org/software/1")
        );
        assert_eq!(rewrite_iri("https://old.example.orgx/software/1", OLD, NEW), None);
        assert_eq!(rewrite_iri(OLD, OLD, NEW).as_deref(), Some(NEW), "the catalog itself");
        assert_eq!(rewrite_iri("https://elsewhere.org/software/1", OLD, NEW), None);
    }

    #[test]
    fn text_and_form_bodies_are_translated() {
        let json = format!(r#"{{"artifact":"{OLD}/artifact/1","other":"https://old.example.org.evil/x"}}"#);
        assert_eq!(
            rewrite_text(&json, OLD, NEW),
            r#"{"artifact":"https://reg.example.org/artifact/1","other":"https://old.example.org.evil/x"}"#
        );
        let form = "iri=https%3A%2F%2Fold.example.org%2Fsoftware%2F1&q=a+b";
        let got = rewrite_form(form, &[OLD.to_string()], NEW);
        let pairs: Vec<(String, String)> = url::form_urlencoded::parse(got.as_bytes()).into_owned().collect();
        assert_eq!(pairs, [("iri".into(), format!("{NEW}/software/1")), ("q".into(), "a b".into())]);
    }

    #[test]
    fn equal_nested_and_non_http_bases_are_refused() {
        assert!(check_bases(OLD, NEW).is_ok());
        assert!(check_bases(OLD, OLD).is_err());
        assert!(check_bases(&format!("{OLD}/"), OLD).is_err(), "a trailing slash is the same base");
        assert!(check_bases(OLD, &format!("{OLD}/registry")).is_err());
        assert!(check_bases(&format!("{NEW}/registry"), NEW).is_err());
        assert!(check_bases("old.example.org", NEW).is_err());
        assert!(check_bases("https://x.org/a", "https://x.org/ab").is_ok(), "a shared string prefix is not nesting");
    }

    #[test]
    fn a_request_to_an_old_host_is_sent_to_the_same_path_under_the_new_base() {
        let req = |host: &str, uri: &str| Request::builder().uri(uri).header("host", host).body(Body::empty()).unwrap();
        let prev = [OLD.to_string(), "http://pilot.example.org:8080/tar".to_string()];
        assert_eq!(
            redirect_target(&req("old.example.org", "/software/1?x=y"), &prev, NEW).as_deref(),
            Some("https://reg.example.org/software/1?x=y")
        );
        assert_eq!(
            redirect_target(&req("pilot.example.org:8080", "/tar/run/9"), &prev, NEW).as_deref(),
            Some("https://reg.example.org/run/9")
        );
        assert_eq!(redirect_target(&req("pilot.example.org:8080", "/tarx/run/9"), &prev, NEW), None);
        assert_eq!(redirect_target(&req("reg.example.org", "/software/1"), &prev, NEW), None);
    }
}
