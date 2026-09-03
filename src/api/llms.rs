//! `GET /llms.txt` — the agent's front door (<https://llmstxt.org>).
//!
//! Public and unauthenticated by design when reads are public: a file whose entire purpose is
//! to tell an unfamiliar client how to read the registry is worth nothing behind a credential
//! the client does not yet know it needs.

use crate::domain::{instance as instdom, software as swdom, Ctx};
use crate::error::{AppError, AppResult};
use crate::llms::{site_index, IndexEntry, SiteIndex};
use crate::model::Instance;
use crate::ns;
use crate::state::AppState;
use axum::extract::State;
use axum::response::IntoResponse;
use std::sync::Arc;

/// How much of the catalogue the index lists before pointing at the paged API. Software and
/// deployments are a stable set that changes by the week, so a small registry is listed whole.
const CATALOGUE_LIMIT: usize = 100;

/// Artifacts and runs are different in kind: a single busy pipeline produces more of them in a
/// day than the catalogue holds in a year, so listing them the same way would bury the parts
/// of the file that orient a reader under a wall of near-identical rows. These sections are a
/// recent window — enough to show what the registry is currently doing — and everything else is
/// one paged request away.
const RECENT_LIMIT: usize = 20;

/// Deployments of one piece of software, for its markdown page.
pub fn instances_of(ctx: &Ctx, software_iri: &str) -> AppResult<Vec<Instance>> {
    let q = format!(
        "{p}\nSELECT DISTINCT ?s WHERE {{ GRAPH ?g {{ ?s a <{t}> ; tar:instanceOf <{software_iri}> }} \
         FILTER NOT EXISTS {{ GRAPH ?tg {{ ?s tar:tombstoned true }} }} }} ORDER BY DESC(STR(?s)) LIMIT 200",
        p = ns::PREFIXES,
        t = instdom::TYPE_TAR_INSTANCE
    );
    let rows = ctx.state.store.select(&q).map_err(AppError::from)?;
    let mut out = Vec::new();
    for iri in rows.rows.iter().filter_map(|r| r.iri("s")) {
        if let Ok(i) = instdom::load_instance(ctx, &iri) {
            out.push(i);
        }
    }
    Ok(out)
}

/// An index note is a hint, not the record. A README pasted whole would make this file
/// unreadable and push the actual links out of any context window.
const NOTE_CHARS: usize = 160;

fn shorten(note: &str) -> Option<String> {
    let one_line = note.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        return None;
    }
    if one_line.chars().count() <= NOTE_CHARS {
        return Some(one_line);
    }
    // Cut at a word boundary so the note ends mid-sentence rather than mid-word.
    let cut: String = one_line.chars().take(NOTE_CHARS).collect();
    let trimmed = cut.rsplit_once(' ').map(|(head, _)| head).unwrap_or(&cut);
    Some(format!("{}…", trimmed.trim_end_matches(&[',', '.', ';', ':'][..])))
}

/// Titles and IRIs for one section of the index, cheaply: one query, no per-record load.
///
/// `note_alternatives` are tried in order and the first bound one wins. They must not be one
/// property path with `|`: a record carrying both a `dct:abstract` and a `schema:description`
/// then matches twice and appears in the index twice, once under each.
fn entries(
    state: &AppState,
    type_iri: &str,
    title_pattern: &str,
    note_alternatives: &[&str],
    limit: usize,
) -> AppResult<(Vec<IndexEntry>, i64)> {
    let mut note_block = String::new();
    let mut vars = Vec::new();
    for (i, pattern) in note_alternatives.iter().enumerate() {
        note_block.push_str(&format!("OPTIONAL {{ {} }}\n", pattern.replace("?note", &format!("?note{i}"))));
        vars.push(format!("?note{i}"));
    }
    if !vars.is_empty() {
        note_block.push_str(&format!("BIND(COALESCE({}) AS ?note)\n", vars.join(", ")));
    }
    let body = format!(
        "GRAPH ?g {{ ?s a <{type_iri}> . OPTIONAL {{ {title_pattern} }} {note_block} }}\n\
         FILTER NOT EXISTS {{ GRAPH ?tg {{ ?s tar:tombstoned true }} }}"
    );
    let total = super::count(state, &body)?;
    let q = format!(
        // Newest first: every id is a UUIDv7, whose hex sorts by the time it was minted, so
        // ordering on the IRI *is* ordering on when the record was created.
        "{p}\nSELECT DISTINCT ?s ?title ?note WHERE {{ {body} }} ORDER BY DESC(STR(?s)) LIMIT {limit}",
        p = ns::PREFIXES
    );
    let rows = state.store.select(&q).map_err(AppError::from)?;
    let items = rows
        .rows
        .iter()
        .filter_map(|r| {
            let iri = r.iri("s")?;
            let title = r.str("title").unwrap_or_else(|| crate::ids::iri_tail(&iri).to_string());
            Some(IndexEntry { title, iri, note: r.str("note").as_deref().and_then(shorten) })
        })
        .collect();
    Ok((items, total))
}

pub async fn llms_txt(State(state): State<Arc<AppState>>) -> AppResult<impl IntoResponse> {
    let body = super::blocking(move || {
        // tar:Software, not schema:SoftwareApplication: a Release carries the schema.org type
        // too, and indexing on that listed every release as if it were a separate tool.
        let (software, n_sw) = entries(
            &state,
            swdom::TYPE_TAR_SOFTWARE,
            "?s schema:name ?title",
            &["?s dct:abstract ?note", "?s schema:description ?note"],
            CATALOGUE_LIMIT,
        )?;
        let (instances, n_in) = entries(
            &state,
            instdom::TYPE_TAR_INSTANCE,
            "?s rdfs:label ?title",
            &["?s dct:description ?note", "?s dcat:endpointURL ?note"],
            CATALOGUE_LIMIT,
        )?;
        let (artifacts, n_ar) = entries(
            &state,
            crate::domain::artifact::TYPE_DATASET,
            "?s dct:title ?title",
            &["?s dct:description ?note"],
            RECENT_LIMIT,
        )?;
        let (runs, n_ru) = entries(
            &state,
            crate::domain::run::TYPE_ACTIVITY,
            "?s rdfs:label ?title",
            &["?s tar:status ?note"],
            RECENT_LIMIT,
        )?;

        Ok((
            site_index(&SiteIndex {
                title: &state.config.title,
                base: state.base(),
                operator: state.config.operator.as_deref(),
                software,
                instances,
                artifacts,
                runs,
                totals: (n_sw, n_in, n_ar, n_ru),
                sparql_public: state.config.sparql_public,
                mcp_enabled: crate::mcp::McpConfig::from_env().enabled,
            }),
            state.base().to_string(),
        ))
    })
    .await?;
    let (body, base) = body;

    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/markdown; charset=utf-8".to_string()),
            (
                axum::http::header::LINK,
                format!("<{}/api/v1/registry>; rel=\"describedby\"; type=\"application/json\"", base),
            ),
        ],
        body,
    ))
}

/// Render one record as markdown, given its kind. Shared by IRI dereference (`/software/{id}.md`)
/// and by `Accept: text/markdown` on the JSON API, so the two cannot answer differently.
pub fn render_record(
    state: &AppState,
    ctx: &Ctx,
    kind: crate::ids::Kind,
    iri: &str,
    quads: &[oxigraph::model::Quad],
) -> AppResult<String> {
    use crate::ids::Kind;
    Ok(match kind {
        Kind::Software => {
            let sw = swdom::load_software(ctx, iri)?;
            let releases = swdom::list_releases(ctx, iri).unwrap_or_default();
            let deployments = instances_of(ctx, iri).unwrap_or_default();
            crate::llms::software(&sw, &releases, &deployments)
        }
        Kind::Instance => crate::llms::instance(&instdom::load_instance(ctx, iri)?),
        Kind::Artifact => crate::llms::artifact(&crate::domain::artifact::load_artifact(ctx, iri)?),
        Kind::Run => crate::llms::run(&crate::domain::run::load_run(ctx, iri)?),
        Kind::Release => {
            let p = crate::rdf::Props::from_quads(iri, quads);
            crate::llms::release(&swdom::release_from_props(ctx, iri, &p))
        }
        // No bespoke renderer for capabilities, agents and the like: they are small, and a
        // Turtle block is more honest than prose invented around three triples.
        _ => format!(
            "# {iri}\n\n> A record in a Tool Artifact Registry with no prose rendering of its \
             own. The Turtle below is the whole of it.\n\n```turtle\n{}\n```\n",
            crate::negotiate::serialize(quads, crate::negotiate::Repr::Turtle, state.base())?
        ),
    })
}
