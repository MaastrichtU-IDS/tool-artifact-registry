//! Executing a tool call.
//!
//! Every tool here turns into an ordinary HTTP request dispatched through
//! [`crate::api::router`] in the same process, carrying the caller's own `Authorization`
//! header verbatim. Nothing about a registry operation — authorisation, SHACL validation,
//! IRI minting, the audit log, federation — is reimplemented; the REST handler does all of it,
//! and the MCP layer is a translator on either side of it.
//!
//! That is deliberate, and it is the answer to "a tool call must never be able to do more than
//! that credential could do through the REST API". There is no second authorisation path to
//! keep in step with the first: `require_curator()` runs in `api::software::create` whether the
//! request arrived from `curl` or from a model.

use super::tools;
use crate::auth::Principal;
use crate::state::AppState;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use tower::ServiceExt;

/// What a tool call produced, before it is wrapped in a `CallToolResult`.
pub struct Outcome {
    pub text: String,
    pub structured: Option<Value>,
    pub is_error: bool,
}

impl Outcome {
    fn ok(text: impl Into<String>, structured: Value) -> Self {
        Self { text: text.into(), structured: Some(structured), is_error: false }
    }
    fn err(text: impl Into<String>) -> Self {
        Self { text: text.into(), structured: None, is_error: true }
    }
}

// ------------------------------------------------------- the internal REST call

fn enc(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

/// Build a query string from the pairs whose value is present and non-empty.
fn query(pairs: Vec<(&str, Option<String>)>) -> String {
    let parts: Vec<String> = pairs
        .into_iter()
        .filter_map(|(k, v)| v.filter(|s| !s.is_empty()).map(|v| format!("{k}={}", enc(&v))))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// Dispatch one request through the registry's own router.
///
/// The router is rebuilt per call rather than cached. It is a few dozen route registrations
/// against work that is dominated by SPARQL evaluation, and the alternative — caching a
/// `Router` keyed by `AppState` — would either leak across the several states a test process
/// holds or need a field on `AppState`, which is not this module's to add.
async fn rest(
    state: &Arc<AppState>,
    auth: Option<&str>,
    method: &str,
    path_and_query: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path_and_query).header("accept", "application/json");
    if let Some(a) = auth {
        b = b.header("authorization", a);
    }
    let req = match &body {
        Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    };
    let Ok(req) = req else {
        return (StatusCode::INTERNAL_SERVER_ERROR, json!({ "detail": "could not build the internal request" }));
    };

    let resp = match crate::api::router(state.clone()).oneshot(req).await {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, json!({ "detail": format!("internal dispatch failed: {e}") }))
        }
    };
    let status = resp.status();
    let bytes = match resp.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, json!({ "detail": e.to_string() })),
    };
    let value = serde_json::from_slice::<Value>(&bytes)
        .unwrap_or_else(|_| json!({ "detail": String::from_utf8_lossy(&bytes).chars().take(500).collect::<String>() }));
    (status, value)
}

/// Turn a registry error response into something a model can act on.
///
/// The 422 case is the one that matters: the registry validates every write against its SHACL
/// shapes and returns an RFC 9457 problem document whose `detail` is already
/// `field: message; field: message`, built from `tar:jsonField` on each result. Handing that
/// back verbatim, with the instruction to fix or drop the named field, closes a correction loop
/// that works — the model retries with the one field changed rather than re-guessing the record.
fn problem_to_message(status: StatusCode, body: &Value) -> String {
    let detail = body
        .get("detail")
        .and_then(Value::as_str)
        .or_else(|| body.get("title").and_then(Value::as_str))
        .unwrap_or("no detail given");
    match status {
        StatusCode::UNPROCESSABLE_ENTITY => format!(
            "The registry refused this write: it does not satisfy the registry's SHACL shapes.\n\n\
             Offending fields: {detail}\n\n\
             Fix exactly the named field(s) and retry. If you cannot establish the true value of a \
             field, remove it from the request rather than substituting a plausible one — the \
             registry renders an absent field honestly. If the field is a vocabulary IRI, get a real \
             one from `vocab_search`. If the field is a closed value set, get the allowed values from \
             `list_enumerations`."
        ),
        StatusCode::FORBIDDEN => format!(
            "Refused: {detail}\n\nThis is an authorisation limit on your credential, not a mistake in \
             the arguments. Do not retry with different arguments; report it, or ask for the role or \
             scope you are missing."
        ),
        StatusCode::UNAUTHORIZED => format!(
            "The credential on this request was rejected by the registry: {detail}"
        ),
        StatusCode::NOT_FOUND => format!(
            "Not found: {detail}\n\nCheck the id with `search_registry` or `list_records` — it may \
             belong to another registry, or be an id you assumed rather than read."
        ),
        StatusCode::CONFLICT => format!("Conflict: {detail}"),
        StatusCode::BAD_REQUEST => format!("The registry rejected the arguments: {detail}"),
        s => format!("The registry returned {}: {detail}", s.as_u16()),
    }
}

// -------------------------------------------------------- the vocabulary guard

/// Which kind of vocabulary term an argument position expects.
///
/// The distinction matters because the registry's two branches are drawn from two different
/// vocabularies — `topic` from EuroSciVoc, `data` from EDAM — and an IRI that is a perfectly
/// real term in the wrong branch is the failure mode a plain existence check misses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    /// What a piece of software is *about*: `vocab_search` with `branch=topic`.
    Topic,
    /// What an artifact *is*: `vocab_search` with `branch=data`, or a locally minted type.
    Type,
}

/// Argument keys whose values are ontology IRIs, at any depth in the argument object.
const VOCAB_KEYS: [(&str, Slot); 5] = [
    ("topics", Slot::Topic),
    ("topic", Slot::Topic),
    ("conforms_to", Slot::Type),
    ("produces", Slot::Type),
    ("consumes", Slot::Type),
];

fn collect_vocab_iris(v: &Value, out: &mut Vec<(String, Slot)>) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if let Some((_, slot)) = VOCAB_KEYS.iter().find(|(key, _)| *key == k.as_str()) {
                    match val {
                        Value::String(s) => out.push((s.clone(), *slot)),
                        Value::Array(a) => {
                            out.extend(a.iter().filter_map(Value::as_str).map(|s| (s.to_string(), *slot)))
                        }
                        _ => {}
                    }
                }
                collect_vocab_iris(val, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_vocab_iris(x, out)),
        _ => {}
    }
}

/// What the registry knows about each IRI: whether it exists at all, and which branch it is in.
///
/// A direct read of the graph rather than a call to `/api/v1/vocab/resolve`, because that
/// endpoint deliberately falls back to the IRI's last path segment as a display label, so
/// "resolved" and "invented" are indistinguishable in its output. This is the one place the MCP
/// layer touches the store directly; it is a read of the public vocabulary graph, not an
/// operation, and no authorisation decision is taken from it.
fn vocabulary_facts(
    state: &Arc<AppState>,
    iris: &[String],
) -> Option<std::collections::HashMap<String, Option<String>>> {
    let values = iris.iter().map(|i| format!("<{i}>")).collect::<Vec<_>>().join(" ");
    let q = format!(
        "{p}\nSELECT DISTINCT ?t ?branch WHERE {{\n  VALUES ?t {{ {values} }}\n\
         \x20 GRAPH ?g {{ ?t ?p ?o . OPTIONAL {{ ?t tar:conceptBranch ?branch }} }}\n}}",
        p = crate::ns::PREFIXES
    );
    let rows = state.store.select(&q).ok()?;
    let mut out: std::collections::HashMap<String, Option<String>> = Default::default();
    for row in rows.rows {
        if let Some(t) = row.iri("t") {
            let e = out.entry(t).or_insert(None);
            if e.is_none() {
                *e = row.str("branch");
            }
        }
    }
    Some(out)
}

/// The measure that holds when the model has not read the tool description.
///
/// Every ontology IRI in the arguments is checked against the registry's vocabulary graph before
/// the write is attempted, on two counts.
///
/// **Does it exist?** An IRI in a vocabulary the registry bundles (EDAM, EuroSciVoc) or minted
/// itself (`{base}/type/…`) that resolves to nothing is **fatal**: the registry is authoritative
/// about the contents of those, so a term it does not contain is not a term, and the likeliest
/// way to arrive at one is a model assembling `topic_` or `data_` plus a remembered number.
/// Anything else is only a **warning** — a foreign type IRI belonging to another registry is
/// legitimate by design (spec D11: an ArtifactType is any IRI), and refusing it would break
/// federation to prevent a mistake it cannot make.
///
/// **Is it the right kind of thing?** This is the subtler half, and it was found by pointing a
/// real coding agent at this server: told to guess, it produced `edamontology.org/topic_3170`,
/// which *does* exist — it is EDAM's "RNA-Seq" — and an existence check waved it through onto a
/// record that has nothing to do with RNA-Seq. But software topics come from EuroSciVoc now, and
/// `build.rs` marks EDAM's topic branch `topic-edam` precisely so the picker never offers it. So
/// the rule is not "does this term exist" but **"could `vocab_search` have returned this term for
/// this field"** — which is exactly the promise the tool descriptions make, and which rejects a
/// real term in the wrong branch as firmly as an invented one.
fn guard_vocabulary(state: &Arc<AppState>, args: &Value) -> Result<Vec<String>, String> {
    let mut found: Vec<(String, Slot)> = Vec::new();
    collect_vocab_iris(args, &mut found);
    found.retain(|(i, _)| i.starts_with("http"));
    found.sort();
    found.dedup();
    found.truncate(100);
    if found.is_empty() {
        return Ok(Vec::new());
    }

    let iris: Vec<String> = found.iter().map(|(i, _)| i.clone()).collect();
    // A failure to check is not evidence of a bad IRI; say so rather than blocking a write for
    // a reason the model cannot act on.
    let Some(facts) = vocabulary_facts(state, &iris) else {
        return Ok(vec!["the vocabulary index could not be read, so these IRIs were written unchecked".into()]);
    };

    let mut fatal: Vec<String> = Vec::new();
    let mut warnings = Vec::new();
    for (iri, slot) in &found {
        let source = crate::domain::type_source(state.base(), iri);
        // Only vocabularies this registry is authoritative for are judged strictly.
        let ours = matches!(source.as_str(), "edam" | "euroscivoc" | "local");

        let Some(branch) = facts.get(iri) else {
            if ours {
                fatal.push(format!(
                    "{iri} does not exist — it claims to be from {source}, which this registry bundles, so \
                     it would know. Search for the real term with `vocab_search`."
                ));
            } else {
                warnings.push(format!(
                    "{iri} is not a term this registry can resolve, so it will render as a bare IRI with no \
                     label. That is legitimate for a type another registry owns; if you meant a term from \
                     this registry's own vocabulary, find the real one with `vocab_search`."
                ));
            }
            continue;
        };

        if !ours {
            continue;
        }
        match (slot, branch.as_deref()) {
            (Slot::Topic, Some("topic")) => {}
            (Slot::Type, Some("data") | None) => {}
            (Slot::Topic, Some("topic-edam")) => fatal.push(format!(
                "{iri} is an EDAM topic. EDAM's topic branch is bundled only so that older records citing \
                 one still render a label; software is classified with EuroSciVoc here, and \
                 `vocab_search` with branch=topic never returns an EDAM topic. Search again with \
                 branch=topic and use what it gives you."
            )),
            (Slot::Topic, other) => fatal.push(format!(
                "{iri} is not a topic (it is in the {} branch). A topic is what a piece of software is \
                 *about*: search with `vocab_search` branch=topic.",
                other.unwrap_or("untyped")
            )),
            (Slot::Type, other) => fatal.push(format!(
                "{iri} is a topic ({}), not an artifact type. An artifact type is what a piece of data \
                 *is*: search with `vocab_search` branch=data, or mint one with `register_artifact_type`.",
                other.unwrap_or("untyped")
            )),
        }
    }

    if !fatal.is_empty() {
        return Err(format!(
            "Refused before writing anything — {} vocabulary problem(s) in these arguments:\n- {}\n\n\
             Every one of these is a term `vocab_search` could not have given you for the field you put it \
             in, which means it was recalled rather than looked up. Search, take an `iri` from a result \
             verbatim, and retry. If nothing matches, omit the field, or mint a local type with \
             `register_artifact_type`. Do not adjust the identifier and try again.",
            fatal.len(),
            fatal.join("\n- ")
        ));
    }
    Ok(warnings)
}

// -------------------------------------------------------------- argument access

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn num_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_i64()).map(|n| n.to_string())
}

fn bool_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

/// A record id for a path position: accept either a bare local id or a full IRI.
fn path_id(arg: &str) -> String {
    let id = if arg.starts_with("http://") || arg.starts_with("https://") {
        crate::ids::iri_tail(arg)
    } else {
        arg
    };
    enc(id)
}

/// Copy the argument keys that map one-to-one onto a REST request body.
fn body_from(args: &Value, keys: &[&str]) -> Value {
    let mut out = Map::new();
    if let Some(map) = args.as_object() {
        for k in keys {
            if let Some(v) = map.get(*k) {
                if !v.is_null() {
                    out.insert((*k).to_string(), v.clone());
                }
            }
        }
    }
    Value::Object(out)
}

// ------------------------------------------------------------------- the tools

/// Execute one tool call.
///
/// `auth` is the raw `Authorization` header value from the MCP request, forwarded unchanged.
pub async fn call(
    state: &Arc<AppState>,
    principal: &Principal,
    auth: Option<&str>,
    name: &str,
    args: &Value,
    read_only: bool,
) -> Outcome {
    let Some(tool) = tools::find(name) else {
        return Outcome::err(format!(
            "There is no tool called `{name}` on this server. Call `tools/list` for the tools your \
             credential can use."
        ));
    };

    if tool.write && read_only {
        return Outcome::err(
            "This registry's MCP endpoint is running in read-only mode, so no write tool is available. \
             Reads still work. Ask the registry operator to unset TAR_MCP_READ_ONLY."
                .to_string(),
        );
    }

    // A pre-check purely so the refusal is a sentence the model can act on. The binding check
    // is the REST handler's own, a few lines below.
    if !tool.gate.allows(principal) {
        return Outcome::err(format!("Refused: {}", tool.gate.refusal(principal)));
    }

    let mut warnings = match guard_vocabulary(state, args) {
        Ok(w) => w,
        Err(fatal) => return Outcome::err(fatal),
    };

    let mut outcome = match name {
        "registry_info" => registry_info(state, principal, auth, read_only).await,
        "vocab_search" => vocab_search(state, auth, args).await,
        "vocab_resolve" => vocab_resolve(state, auth, args).await,
        "list_enumerations" => list_enumerations(),
        "register_artifact_type" => simple_write(
            state,
            auth,
            "POST",
            "/api/v1/types".into(),
            body_from(args, &["label", "definition", "default_media_type", "slug"]),
            "Artifact type registered. Use its `iri` for conforms_to, produces and consumes.",
        )
        .await,
        "search_registry" => search_registry(state, auth, args).await,
        "list_records" => list_records(state, auth, args).await,
        "get_record" => get_record(state, auth, args).await,
        "find_capable_software" => find_capable(state, auth, args).await,
        "get_artifact_lineage" => lineage(state, auth, args).await,
        "register_software" => register_software(state, auth, args).await,
        "update_software" => update_software(state, auth, args).await,
        "add_release" => add_release(state, auth, args).await,
        "declare_capability" => declare_capability(state, auth, args).await,
        "register_instance" => simple_write(
            state,
            auth,
            "POST",
            "/api/v1/instances".into(),
            body_from(
                args,
                &[
                    "label", "software", "release", "endpoint_url", "endpoint_description", "description",
                    "operator", "availability", "jurisdiction", "oidc_client_id", "oidc_issuer",
                    "allowed_scopes", "capability",
                ],
            ),
            "Deployment registered.",
        )
        .await,
        "advertise_produced" => simple_write(
            state,
            auth,
            "POST",
            "/api/v1/advertise/produced".into(),
            body_from(args, &["run", "artifacts"]),
            "Advertised. The run and its outputs are now in the lineage graph.",
        )
        .await,
        "advertise_consumed" => simple_write(
            state,
            auth,
            "POST",
            "/api/v1/advertise/consumed".into(),
            body_from(args, &["run", "artifacts"]),
            "Advertised. The run's inputs are now in the lineage graph.",
        )
        .await,
        other => Outcome::err(format!("tool `{other}` is listed but not wired up — this is a bug in the registry")),
    };

    if !warnings.is_empty() && !outcome.is_error {
        warnings.insert(0, "Warnings:".to_string());
        outcome.text = format!("{}\n\n{}", outcome.text, warnings.join("\n- "));
    }
    outcome
}

async fn registry_info(
    state: &Arc<AppState>,
    principal: &Principal,
    auth: Option<&str>,
    read_only: bool,
) -> Outcome {
    let (_, registry) = rest(state, auth, "GET", "/api/v1/registry", None).await;
    let (_, well_known) = rest(state, auth, "GET", "/.well-known/tar-registry", None).await;
    let (_, whoami) = rest(state, auth, "GET", "/api/v1/whoami", None).await;
    let allowed: Vec<&str> = tools::visible(principal, read_only).iter().map(|t| t.name).collect();

    let counts = registry.get("counts").cloned().unwrap_or(json!({}));
    let text = format!(
        "{} — {}\n\nHolds: {}\n\nYou are authenticated as `{}` ({}){}.\nRoles: {}. Scopes: {}.\n\n\
         Tools you may call: {}.{}",
        registry.get("title").and_then(Value::as_str).unwrap_or("registry"),
        state.base(),
        serde_json::to_string(&counts).unwrap_or_default(),
        principal.subject,
        principal.actor_kind(),
        principal
            .instance_iri
            .as_deref()
            .map(|i| format!(", acting as the deployment {i}"))
            .unwrap_or_default(),
        if principal.roles.is_empty() { "none".into() } else { format!("{:?}", principal.roles) },
        if principal.scopes.is_empty() {
            "none".to_string()
        } else {
            principal.scopes.iter().cloned().collect::<Vec<_>>().join(" ")
        },
        allowed.join(", "),
        if read_only { "\n\nThis endpoint is in read-only mode." } else { "" }
    );
    Outcome::ok(
        text,
        json!({
            "registry": registry,
            "auth": well_known.get("auth").cloned().unwrap_or(json!({})),
            "you": whoami,
            "available_tools": allowed,
            "read_only": read_only,
        }),
    )
}

async fn vocab_search(state: &Arc<AppState>, auth: Option<&str>, args: &Value) -> Outcome {
    let q = str_arg(args, "q").unwrap_or_default();
    let path = format!(
        "/api/v1/vocab/search{}",
        query(vec![
            ("q", Some(q.to_string())),
            ("branch", str_arg(args, "branch").map(String::from)),
            ("limit", num_arg(args, "limit")),
        ])
    );
    let (status, body) = rest(state, auth, "GET", &path, None).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &body));
    }
    let items = body.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    if items.is_empty() {
        return Outcome::ok(
            format!(
                "No vocabulary term matches {q:?}. Try other wordings before concluding there is none — \
                 vocabulary labels are often more formal than everyday usage. If there genuinely is no term, \
                 either omit the field or mint a local artifact type with `register_artifact_type`. Do \
                 not write an ontology IRI that did not come from this tool."
            ),
            body,
        );
    }
    let lines: Vec<String> = items
        .iter()
        .map(|h| {
            format!(
                "- {} — {} [{}]{}",
                h.get("label").and_then(Value::as_str).unwrap_or("?"),
                h.get("iri").and_then(Value::as_str).unwrap_or("?"),
                h.get("source").and_then(Value::as_str).unwrap_or("?"),
                h.get("definition")
                    .and_then(Value::as_str)
                    .map(|d| format!("\n    {}", d.chars().take(180).collect::<String>()))
                    .unwrap_or_default()
            )
        })
        .collect();
    Outcome::ok(
        format!("{} match(es) for {q:?}. Use an `iri` below verbatim.\n{}", items.len(), lines.join("\n")),
        body,
    )
}

async fn vocab_resolve(state: &Arc<AppState>, auth: Option<&str>, args: &Value) -> Outcome {
    let iris: Vec<String> = args
        .get("iris")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default();
    if iris.is_empty() {
        return Outcome::err("give at least one IRI in `iris`".to_string());
    }
    let path = format!("/api/v1/vocab/resolve{}", query(vec![("iris", Some(iris.join(",")))]));
    let (status, body) = rest(state, auth, "GET", &path, None).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &body));
    }
    // `/vocab/resolve` falls back to the IRI's last path segment as a display label, so its
    // output cannot distinguish "resolved" from "invented" — ask the graph directly.
    let known = vocabulary_facts(state, &iris).unwrap_or_default();
    let unresolved: Vec<&String> = iris.iter().filter(|i| !known.contains_key(*i)).collect();
    let note = if unresolved.is_empty() {
        "All resolved.".to_string()
    } else {
        format!(
            "Unresolved by this registry: {}. Any of those in a vocabulary the registry bundles (EDAM, \
             EuroSciVoc) or minted itself is not a real term and will be refused on write.",
            unresolved.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        )
    };
    Outcome::ok(note, body)
}

fn list_enumerations() -> Outcome {
    // Sourced from `shapes/tar-shapes.ttl`, `crate::auth` and `crate::model`, so this cannot
    // drift into describing values the SHACL shapes would reject.
    let data = json!({
        "software_kinds": {
            "field": "kinds (register_software, update_software)",
            "note": "A set, not a single choice: one program is routinely several of these.",
            "values": {
                "service": "runs as a hosted service with an endpoint",
                "library": "imported into other code",
                "cli": "a command-line program",
                "desktop": "a graphical application a person installs",
                "workflow": "a pipeline or workflow definition",
            }
        },
        "maturity": {
            "field": "maturity (register_software, update_software)",
            "note": "repostatus.org development status. Only set it if the project declares one.",
            "values": ["concept", "wip", "active", "inactive", "unsupported", "suspended", "abandoned", "moved"],
        },
        "availability": {
            "field": "availability (register_instance, distributions, release downloads)",
            "values": {
                "public": "anyone can get it",
                "restricted": "access needs an agreement or an account",
                "embargoed": "will become available later",
                "metadata-only": "the record describes something whose bytes are not obtainable here — the honest value when there is no URL",
            }
        },
        "access_protocol": {
            "field": "distributions[].access_protocol",
            "values": ["https", "http", "s3", "sparql", "oci", "ipfs", "file"],
        },
        "auth_method": {
            "field": "distributions[].auth_method",
            "values": ["none", "apikey", "oauth2", "basic", "signed-url"],
        },
        "run_status": {
            "field": "run.status (advertise_produced, advertise_consumed)",
            "values": ["success", "failed", "running", "aborted"],
        },
        "instance_health": {
            "field": "status (list_records, kind=instance)",
            "values": ["up", "down", "unknown"],
            "note": "Observed by the registry, not written by a caller.",
        },
        "scopes": {
            "field": "allowed_scopes (register_instance); also what your own credential carries",
            "values": {
                "advertise:produce": "advertise artifacts a run produced",
                "advertise:consume": "advertise artifacts a run consumed",
                "register:software": "register and update software",
                "register:instance": "register deployments",
                "read:private": "read records that are not publicly readable",
                "admin:*": "everything",
            }
        },
        "agent_kind": { "field": "publisher.kind, operator.kind, creators[].kind", "values": ["person", "organization"] },
        "lineage_direction": { "field": "direction (get_artifact_lineage)", "values": ["up", "down", "both"] },
        "vocab_branch": {
            "field": "branch (vocab_search)",
            "values": { "topic": "what software is about", "data": "what an artifact is" },
        },
        "record_kinds": { "field": "kind (get_record, list_records)", "values": ["software", "instance", "artifact", "run", "release", "type"] },
        "syncable_fields": {
            "field": "which software fields a connected forge may overwrite",
            "values": crate::model::SYNCABLE_FIELDS,
        },
    });
    Outcome::ok(
        "The registry's closed value sets. Anything outside these is rejected by SHACL validation. \
         Licences are not enumerated: use an SPDX IRI such as https://spdx.org/licenses/Apache-2.0, and \
         only when the project actually states one."
            .to_string(),
        data,
    )
}

async fn search_registry(state: &Arc<AppState>, auth: Option<&str>, args: &Value) -> Outcome {
    let path = format!(
        "/api/v1/search{}",
        query(vec![
            ("q", str_arg(args, "q").map(String::from)),
            ("type", str_arg(args, "type").map(String::from)),
            ("federated", bool_arg(args, "federated").filter(|b| *b).map(|_| "true".to_string())),
            ("limit", num_arg(args, "limit")),
        ])
    );
    let (status, body) = rest(state, auth, "GET", &path, None).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &body));
    }
    let n = body.get("items").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
    Outcome::ok(format!("{n} hit(s)."), body)
}

async fn list_records(state: &Arc<AppState>, auth: Option<&str>, args: &Value) -> Outcome {
    let kind = str_arg(args, "kind").unwrap_or("software");
    let paging = vec![("cursor", str_arg(args, "cursor").map(String::from)), ("limit", num_arg(args, "limit"))];
    let mut pairs = paging;
    let base_path = match kind {
        "software" => {
            pairs.extend(vec![
                ("q", str_arg(args, "q").map(String::from)),
                ("license", str_arg(args, "license").map(String::from)),
                ("publisher", str_arg(args, "publisher").map(String::from)),
                ("topic", str_arg(args, "topic").map(String::from)),
                ("keyword", str_arg(args, "keyword").map(String::from)),
                ("kind", str_arg(args, "kind_filter").map(String::from)),
                ("produces", str_arg(args, "produces").map(String::from)),
                ("consumes", str_arg(args, "consumes").map(String::from)),
                ("registry", str_arg(args, "registry").map(String::from)),
            ]);
            "/api/v1/software".to_string()
        }
        "instance" => {
            pairs.extend(vec![
                ("q", str_arg(args, "q").map(String::from)),
                ("software", str_arg(args, "software").map(String::from)),
                ("release", str_arg(args, "release").map(String::from)),
                ("operator", str_arg(args, "publisher").map(String::from)),
                ("status", str_arg(args, "status").map(String::from)),
                ("registry", str_arg(args, "registry").map(String::from)),
            ]);
            "/api/v1/instances".to_string()
        }
        "artifact" => {
            pairs.extend(vec![
                ("q", str_arg(args, "q").map(String::from)),
                ("conforms_to", str_arg(args, "conforms_to").map(String::from)),
                ("license", str_arg(args, "license").map(String::from)),
                ("availability", str_arg(args, "availability").map(String::from)),
                ("instance", str_arg(args, "instance").map(String::from)),
                ("software", str_arg(args, "software").map(String::from)),
                ("run", str_arg(args, "run").map(String::from)),
                ("registry", str_arg(args, "registry").map(String::from)),
            ]);
            "/api/v1/artifacts".to_string()
        }
        "run" => {
            pairs.extend(vec![
                ("q", str_arg(args, "q").map(String::from)),
                ("instance", str_arg(args, "instance").map(String::from)),
                ("software", str_arg(args, "software").map(String::from)),
                ("status", str_arg(args, "status").map(String::from)),
            ]);
            "/api/v1/runs".to_string()
        }
        "type" => "/api/v1/types".to_string(),
        "release" => {
            let Some(sw) = str_arg(args, "software") else {
                return Outcome::err(
                    "listing releases needs `software` — the id or IRI of the software whose releases you want"
                        .to_string(),
                );
            };
            format!("/api/v1/software/{}/releases", path_id(sw))
        }
        other => {
            return Outcome::err(format!(
                "`{other}` is not a record kind. Use one of software, instance, artifact, run, release, type."
            ))
        }
    };

    let (status, body) = rest(state, auth, "GET", &format!("{base_path}{}", query(pairs)), None).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &body));
    }
    let n = body.get("items").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
    let total = body.get("total").and_then(Value::as_i64);
    let more = body.get("next_cursor").and_then(Value::as_str);
    Outcome::ok(
        format!(
            "{n} {kind} record(s){}{}",
            total.map(|t| format!(" of {t}")).unwrap_or_default(),
            more.map(|c| format!(". More available — pass cursor={c}.")).unwrap_or_else(|| ".".into())
        ),
        body,
    )
}

async fn get_record(state: &Arc<AppState>, auth: Option<&str>, args: &Value) -> Outcome {
    let Some(id) = str_arg(args, "id") else { return Outcome::err("`id` is required".to_string()) };
    let kind = str_arg(args, "kind").unwrap_or("software");
    let path = match kind {
        "software" => format!("/api/v1/software/{}", path_id(id)),
        "instance" => format!("/api/v1/instances/{}", path_id(id)),
        "artifact" => format!("/api/v1/artifacts/{}", path_id(id)),
        "run" => format!("/api/v1/runs/{}", path_id(id)),
        "type" => format!("/api/v1/types/{}", path_id(id)),
        other => {
            return Outcome::err(format!(
                "`{other}` is not a readable record kind here. Use software, instance, artifact, run or \
                 type; for one release, list the software's releases with `list_records`."
            ))
        }
    };
    let (status, body) = rest(state, auth, "GET", &path, None).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &body));
    }
    Outcome::ok(format!("The {kind} record for {id}."), body)
}

async fn find_capable(state: &Arc<AppState>, auth: Option<&str>, args: &Value) -> Outcome {
    let produces = str_arg(args, "produces").map(String::from);
    let consumes = str_arg(args, "consumes").map(String::from);
    if produces.is_none() && consumes.is_none() {
        return Outcome::err(
            "give at least one of `produces` or `consumes`, as an artifact type IRI from `vocab_search`"
                .to_string(),
        );
    }
    let path = format!("/api/v1/capabilities{}", query(vec![("produces", produces), ("consumes", consumes)]));
    let (status, body) = rest(state, auth, "GET", &path, None).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &body));
    }
    let n = body.get("items").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
    Outcome::ok(
        if n == 0 {
            "Nothing declares that capability here. That may mean nobody has declared it yet rather \
             than that nothing can do it — a capability has to be written down to be found."
                .to_string()
        } else {
            format!("{n} match(es).")
        },
        body,
    )
}

async fn lineage(state: &Arc<AppState>, auth: Option<&str>, args: &Value) -> Outcome {
    let Some(id) = str_arg(args, "id") else { return Outcome::err("`id` is required".to_string()) };
    let path = format!(
        "/api/v1/artifacts/{}/lineage{}",
        path_id(id),
        query(vec![("depth", num_arg(args, "depth")), ("direction", str_arg(args, "direction").map(String::from))])
    );
    let (status, body) = rest(state, auth, "GET", &path, None).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &body));
    }
    Outcome::ok(format!("Lineage around {id}."), body)
}

const SOFTWARE_FIELDS: [&str; 18] = [
    "name", "tagline", "description", "homepage", "code_repository", "documentation", "download_url",
    "readme", "readme_base_url", "image", "license", "kinds", "maturity", "deployable", "topics",
    "keywords", "publications", "capability",
];

async fn register_software(state: &Arc<AppState>, auth: Option<&str>, args: &Value) -> Outcome {
    let mut body = body_from(args, &SOFTWARE_FIELDS);
    for k in ["publisher", "contact"] {
        if let Some(v) = args.get(k) {
            body.as_object_mut().unwrap().insert(k.into(), v.clone());
        }
    }
    let (status, resp) = rest(state, auth, "POST", "/api/v1/software", Some(body)).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &resp));
    }
    Outcome::ok(
        format!(
            "Registered as {}. Fields you left out are shown as not stated, which is what they are — \
             fill them in later from evidence rather than now from inference.",
            resp.get("iri").and_then(Value::as_str).unwrap_or("(no iri)")
        ),
        resp,
    )
}

async fn update_software(state: &Arc<AppState>, auth: Option<&str>, args: &Value) -> Outcome {
    let Some(id) = str_arg(args, "id") else { return Outcome::err("`id` is required".to_string()) };
    let mut body = body_from(args, &SOFTWARE_FIELDS);
    for k in ["publisher", "contact"] {
        if let Some(v) = args.get(k) {
            body.as_object_mut().unwrap().insert(k.into(), v.clone());
        }
    }
    let (status, resp) =
        rest(state, auth, "PATCH", &format!("/api/v1/software/{}", path_id(id)), Some(body)).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &resp));
    }
    Outcome::ok("Updated.".to_string(), resp)
}

async fn add_release(state: &Arc<AppState>, auth: Option<&str>, args: &Value) -> Outcome {
    let Some(sw) = str_arg(args, "software") else { return Outcome::err("`software` is required".to_string()) };
    let body = body_from(
        args,
        &[
            "version", "date_published", "container_image", "image_digest", "changelog", "install_command",
            "downloads", "capability",
        ],
    );
    let (status, resp) =
        rest(state, auth, "POST", &format!("/api/v1/software/{}/releases", path_id(sw)), Some(body)).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &resp));
    }
    Outcome::ok("Release recorded.".to_string(), resp)
}

async fn declare_capability(state: &Arc<AppState>, auth: Option<&str>, args: &Value) -> Outcome {
    let Some(id) = str_arg(args, "id") else { return Outcome::err("`id` is required".to_string()) };
    let target = str_arg(args, "target").unwrap_or("software");
    let path = match target {
        "software" => format!("/api/v1/software/{}/capability", path_id(id)),
        "instance" => format!("/api/v1/instances/{}/capability", path_id(id)),
        other => return Outcome::err(format!("`target` must be software or instance, not `{other}`")),
    };
    let body = body_from(args, &["produces", "consumes"]);
    let (status, resp) = rest(state, auth, "PUT", &path, Some(body)).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &resp));
    }
    Outcome::ok(format!("Capability declared on the {target}."), resp)
}

async fn simple_write(
    state: &Arc<AppState>,
    auth: Option<&str>,
    method: &str,
    path: String,
    body: Value,
    success: &str,
) -> Outcome {
    let (status, resp) = rest(state, auth, method, &path, Some(body)).await;
    if !status.is_success() {
        return Outcome::err(problem_to_message(status, &resp));
    }
    Outcome::ok(success.to_string(), resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocab_iris_are_collected_from_any_depth() {
        let args = json!({
            "run": { "external_key": "x" },
            "artifacts": [{
                "conforms_to": "http://edamontology.org/data_2048",
                "distributions": [{ "conforms_to": "https://reg.example/type/report" }]
            }],
            "capability": { "produces": ["http://edamontology.org/data_1", "http://edamontology.org/data_2"], "consumes": [] }
        });
        let mut out = Vec::new();
        collect_vocab_iris(&args, &mut out);
        out.sort();
        assert_eq!(
            out,
            vec![
                ("http://edamontology.org/data_1".to_string(), Slot::Type),
                ("http://edamontology.org/data_2".to_string(), Slot::Type),
                ("http://edamontology.org/data_2048".to_string(), Slot::Type),
                ("https://reg.example/type/report".to_string(), Slot::Type),
            ]
        );
    }

    #[test]
    fn topics_and_types_are_collected_into_different_slots() {
        let args = json!({ "topics": ["https://a.example/t"], "capability": { "produces": ["https://b.example/x"] } });
        let mut out = Vec::new();
        collect_vocab_iris(&args, &mut out);
        out.sort();
        assert_eq!(
            out,
            vec![
                ("https://a.example/t".to_string(), Slot::Topic),
                ("https://b.example/x".to_string(), Slot::Type),
            ]
        );
    }

    #[test]
    fn keywords_and_free_text_are_not_treated_as_vocabulary() {
        let args = json!({ "keywords": ["http://not-a-term.example/x"], "name": "http://also-not.example" });
        let mut out = Vec::new();
        collect_vocab_iris(&args, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn full_iris_and_bare_ids_both_reach_a_path_segment() {
        assert_eq!(path_id("01J9F"), "01J9F");
        assert_eq!(path_id("https://reg.example/software/01J9F"), "01J9F");
    }

    #[test]
    fn query_strings_skip_absent_and_empty_values() {
        assert_eq!(query(vec![("a", Some("1".into())), ("b", None), ("c", Some(String::new()))]), "?a=1");
        assert_eq!(query(vec![("q", Some("a b&c".into()))]), "?q=a%20b%26c");
    }

    #[test]
    fn a_validation_failure_becomes_a_correction_instruction() {
        let body = json!({ "detail": "license: value is not a valid IRI" });
        let msg = problem_to_message(StatusCode::UNPROCESSABLE_ENTITY, &body);
        assert!(msg.contains("license: value is not a valid IRI"));
        assert!(msg.contains("remove it from the request"));
        assert!(msg.contains("vocab_search"));
    }

    #[test]
    fn a_forbidden_response_tells_the_model_not_to_retry() {
        let msg = problem_to_message(StatusCode::FORBIDDEN, &json!({ "detail": "curator role required" }));
        assert!(msg.contains("Do not retry with different arguments"));
    }

    #[test]
    fn body_from_copies_only_named_keys_and_drops_nulls() {
        let args = json!({ "name": "x", "secret": "y", "license": null });
        let b = body_from(&args, &["name", "license"]);
        assert_eq!(b, json!({ "name": "x" }));
    }
}
