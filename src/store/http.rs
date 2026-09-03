//! A [`GraphStore`] over an external SPARQL 1.1 endpoint — Fuseki, GraphDB, QLever, Virtuoso.
//!
//! Selected by `TAR_SPARQL_ENDPOINT`. Without it the registry uses embedded Oxigraph exactly
//! as before, so nobody running this today has to change anything.
//!
//! Only four methods here are really this backend's own: `select`, `ask`, `construct` and
//! `apply`. Everything the registry reads by subject — `describe`, `exists`, `graph_of`,
//! `count` — comes from the trait's default methods over `select`/`ask`, which is the point of
//! having written them as SPARQL in `store::queries`: the two backends cannot drift apart on
//! the closure without one of them failing to execute a standard query.
//!
//! **This is not `/sparql`.** The registry's public query endpoint is read-only and refuses
//! updates; this is the private connection to the registry's own storage.
//!
//! **Blocking from async.** `GraphStore` is a synchronous trait called from async handlers, so
//! the HTTP work happens on a dedicated thread with its own runtime and the calling thread
//! waits on a channel. `reqwest::blocking` is not usable here (it refuses to run inside a
//! runtime) and `block_in_place` is not either (the tests run on a current-thread runtime).
//! The cost is that a store call occupies its Tokio worker thread for the round trip; for a
//! registry whose alternative is an in-process store that is a real change in concurrency
//! behaviour and worth knowing about before pointing a busy deployment at a remote endpoint.

use anyhow::{anyhow, bail, Context, Result};
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{GraphName, Quad, Triple};
use oxigraph::sparql::results::{QueryResultsFormat, QueryResultsParser, ReaderQueryResultsParserOutput};
use std::collections::BTreeMap;
use std::sync::mpsc::SyncSender;

use crate::config::{SparqlAuth, SparqlBackend};

use super::queries;
use super::{Bindings, GraphStore, GraphTx, Row};

/// How many quads go into one bulk `INSERT DATA`.
///
/// Bulk loading (the bundled vocabularies at boot, a restored dump) is not a transaction and
/// is chunked so a 6000-triple file is not one enormous request body. `apply` is never
/// chunked — see [`HttpSparqlStore::apply`].
const BULK_CHUNK: usize = 2000;

struct Request {
    url: String,
    content_type: &'static str,
    accept: &'static str,
    body: String,
    reply: SyncSender<Result<String>>,
}

pub struct HttpSparqlStore {
    endpoint: SparqlBackend,
    jobs: tokio::sync::mpsc::UnboundedSender<Request>,
}

impl HttpSparqlStore {
    pub fn connect(endpoint: SparqlBackend) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(endpoint.timeout)
            .user_agent(concat!("tool-artifact-registry/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client for the SPARQL backend")?;
        let (jobs, mut rx) = tokio::sync::mpsc::unbounded_channel::<Request>();
        let auth = endpoint.auth.clone();
        std::thread::Builder::new()
            .name("sparql-backend".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        // Every later call reports the closed channel; say why once here.
                        tracing::error!(error = %e, "the SPARQL backend worker could not start");
                        return;
                    }
                };
                rt.block_on(async move {
                    while let Some(req) = rx.recv().await {
                        let (client, auth) = (client.clone(), auth.clone());
                        tokio::spawn(async move {
                            let reply = req.reply.clone();
                            let _ = reply.send(perform(&client, &auth, req).await);
                        });
                    }
                });
            })
            .context("starting the SPARQL backend worker thread")?;
        Ok(Self { endpoint, jobs })
    }

    fn post(&self, url: &str, content_type: &'static str, accept: &'static str, body: String) -> Result<String> {
        let (reply, wait) = std::sync::mpsc::sync_channel(1);
        self.jobs
            .send(Request { url: url.to_string(), content_type, accept, body, reply })
            .map_err(|_| anyhow!("the SPARQL backend worker for {url} has stopped"))?;
        wait.recv().map_err(|_| anyhow!("the SPARQL backend worker for {url} dropped the request"))?
    }

    fn query(&self, sparql: &str, accept: &'static str) -> Result<String> {
        self.post(&self.endpoint.query_endpoint, "application/sparql-query", accept, sparql.to_string())
    }

    fn update(&self, sparql: String) -> Result<String> {
        self.post(&self.endpoint.update_endpoint, "application/sparql-update", "*/*", sparql)
    }

    /// Insert quads in chunks. Bulk load only; not a transaction.
    fn bulk_insert(&self, quads: &[Quad]) -> Result<()> {
        for chunk in quads.chunks(BULK_CHUNK) {
            self.update(insert_data(chunk))?;
        }
        Ok(())
    }

    /// The size delta of a bulk load, which is what the embedded backend reports: quads the
    /// store did not already hold. Two extra counts per load, at boot and on restore only.
    fn load(&self, quads: &[Quad]) -> Result<usize> {
        let before = self.count()?;
        self.bulk_insert(quads)?;
        Ok(self.count()?.saturating_sub(before))
    }
}

async fn perform(client: &reqwest::Client, auth: &SparqlAuth, req: Request) -> Result<String> {
    let mut b = client
        .post(&req.url)
        .header(reqwest::header::CONTENT_TYPE, req.content_type)
        .header(reqwest::header::ACCEPT, req.accept)
        .body(req.body);
    b = match auth {
        SparqlAuth::None => b,
        SparqlAuth::Bearer(t) => b.bearer_auth(t),
        SparqlAuth::Basic { username, password } => b.basic_auth(username, Some(password)),
    };
    // Named, always. A query that comes back empty because the server is down looks exactly
    // like a registry with no records, so this must never degrade into an empty result set.
    let resp = b.send().await.with_context(|| format!("SPARQL endpoint {} is unreachable", req.url))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail: String = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(400).collect();
        bail!("SPARQL endpoint {} returned {status}: {detail}", req.url);
    }
    Ok(text)
}

impl GraphStore for HttpSparqlStore {
    fn select(&self, sparql: &str) -> Result<Bindings> {
        let body = self.query(sparql, "application/sparql-results+json")?;
        let parser = QueryResultsParser::from_format(QueryResultsFormat::Json);
        match parser.for_reader(body.as_bytes()).context("reading SPARQL results")? {
            ReaderQueryResultsParserOutput::Solutions(reader) => {
                let vars: Vec<String> = reader.variables().iter().map(|v| v.as_str().to_string()).collect();
                let mut rows = Vec::new();
                for solution in reader {
                    let solution = solution.context("reading a SPARQL result row")?;
                    let mut map = std::collections::HashMap::new();
                    for v in &vars {
                        if let Some(t) = solution.get(v.as_str()) {
                            map.insert(v.clone(), t.clone());
                        }
                    }
                    rows.push(Row(map));
                }
                Ok(Bindings { vars, rows })
            }
            ReaderQueryResultsParserOutput::Boolean(_) => Err(anyhow!("expected a SELECT query")),
        }
    }

    fn ask(&self, sparql: &str) -> Result<bool> {
        let body = self.query(sparql, "application/sparql-results+json")?;
        let parser = QueryResultsParser::from_format(QueryResultsFormat::Json);
        match parser.for_reader(body.as_bytes()).context("reading SPARQL results")? {
            ReaderQueryResultsParserOutput::Boolean(b) => Ok(b),
            ReaderQueryResultsParserOutput::Solutions(_) => Err(anyhow!("expected an ASK query")),
        }
    }

    fn construct(&self, sparql: &str) -> Result<Vec<Triple>> {
        let body = self.query(sparql, "text/turtle")?;
        let mut out = Vec::new();
        for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(body.as_bytes()) {
            let q = quad.context("parsing the Turtle a CONSTRUCT returned")?;
            out.push(Triple::new(q.subject, q.predicate, q.object));
        }
        Ok(out)
    }

    /// The whole transaction as one SPARQL Update request.
    ///
    /// **Atomicity.** The deletions and the insertion go into a single request body, separated
    /// by `;`. SPARQL 1.1 Update says a request is a sequence of operations, and the servers
    /// this targets (Fuseki, GraphDB, Virtuoso, Oxigraph's own server) execute one request in
    /// one transaction — so a write either lands whole or not at all, which is what the
    /// embedded backend's `start_transaction`/`commit` gives. One HTTP call per operation
    /// would not: a crash between "delete the old distribution" and "insert the new one"
    /// leaves a record with neither.
    ///
    /// That guarantee is the server's, not this code's. A SPARQL endpoint is free to process
    /// operations independently, and against such a server this is atomic only per operation.
    /// The registry cannot detect the difference over HTTP and does not pretend to.
    ///
    /// **Order** is delete-subjects, then delete-properties, then insert — the same order the
    /// embedded backend applies them in, and it matters: `replace_property` exists so that an
    /// insert in the same transaction replaces a value rather than accumulating one.
    ///
    /// **One difference from the embedded backend, stated rather than hidden.** The subject
    /// deletion follows the ownership closure to a fixed [`queries::DEFAULT_DEPTH`] levels,
    /// where the embedded backend's walk is unbounded. `describe` can escalate because it can
    /// look at what came back; a deletion inside a single atomic request cannot, and a query
    /// with 32 nested levels is not one to send on every write. The deepest chain the registry
    /// writes is two levels, so this is headroom rather than a limit — but a sub-resource
    /// nested deeper than four would be orphaned here and removed there.
    fn apply(&self, tx: GraphTx) -> Result<()> {
        if tx.is_empty() {
            return Ok(());
        }
        let mut ops: Vec<String> = Vec::new();
        for (subject, graph) in &tx.delete_subjects {
            let g = queries::iri(graph)?;
            // A pattern delete, not `DELETE DATA`: the closure runs through blank nodes and
            // `DELETE DATA` may not name one. `?s` is bound by the closure body.
            ops.push(format!(
                "DELETE {{ GRAPH {g} {{ ?s ?p ?o }} }}\nWHERE {{\n{}\n}}",
                queries::owned_closure_body(subject, graph, queries::DEFAULT_DEPTH)?
            ));
        }
        for (subject, predicate, graph) in &tx.delete_properties {
            ops.push(format!(
                "DELETE WHERE {{ GRAPH {} {{ {} {} ?o }} }}",
                queries::iri(graph)?,
                queries::iri(subject)?,
                queries::iri(predicate)?
            ));
        }
        if !tx.insert.is_empty() {
            ops.push(insert_data(&tx.insert));
        }
        self.update(ops.join(" ;\n"))?;
        Ok(())
    }

    fn drop_graph(&self, graph: &str) -> Result<()> {
        // SILENT: the embedded backend's `remove_named_graph` on an absent graph is a no-op,
        // and `load_vocab` drops the shapes graph on a fresh store at every boot.
        self.update(format!("DROP SILENT GRAPH {}", queries::iri(graph)?))?;
        Ok(())
    }

    fn dump_nquads(&self, graph: Option<&str>) -> Result<String> {
        let mut out = String::new();
        match graph {
            // One graph: N-Triples, graph name dropped — the same shape the embedded backend
            // serialises, and the format `/admin/dump?graph=` and peer stub exchange expect.
            Some(g) => {
                let q = format!("SELECT ?s ?p ?o WHERE {{ GRAPH {} {{ ?s ?p ?o }} }}", queries::iri(g)?);
                for row in &self.select(&q)?.rows {
                    let (Some(s), Some(p), Some(o)) = (row.term("s"), row.term("p"), row.term("o")) else {
                        continue;
                    };
                    out.push_str(&format!("{s} {p} {o} .\n"));
                }
            }
            None => {
                let q = "SELECT ?g ?s ?p ?o WHERE { GRAPH ?g { ?s ?p ?o } }";
                for row in &self.select(q)?.rows {
                    let (Some(g), Some(s), Some(p), Some(o)) =
                        (row.term("g"), row.term("s"), row.term("p"), row.term("o"))
                    else {
                        continue;
                    };
                    out.push_str(&format!("{s} {p} {o} {g} .\n"));
                }
            }
        }
        Ok(out)
    }

    fn load_turtle(&self, data: &str, graph: &str, base: Option<&str>) -> Result<usize> {
        let g = oxigraph::model::NamedNode::new(graph).map_err(|e| anyhow!("bad IRI {graph}: {e}"))?;
        // Parsed here rather than shipped as Turtle so that the base IRI and the target graph
        // are resolved exactly as the embedded backend resolves them, and so a syntax error is
        // a local error naming the file rather than a 400 from somebody else's server.
        let mut parser = RdfParser::from_format(RdfFormat::Turtle).without_named_graphs().with_default_graph(g);
        if let Some(b) = base {
            parser = parser.with_base_iri(b).map_err(|e| anyhow!("bad base IRI: {e}"))?;
        }
        let quads: Vec<Quad> =
            parser.for_slice(data.as_bytes()).collect::<Result<Vec<_>, _>>().context("parsing Turtle")?;
        self.load(&quads)
    }

    fn load_nquads(&self, data: &str) -> Result<usize> {
        let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
            .for_slice(data.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .context("parsing N-Quads")?;
        self.load(&quads)
    }
}

/// `INSERT DATA` for a quad list, grouped by graph.
///
/// Terms are written by Oxigraph's own N-Triples `Display`, which is where the escaping comes
/// from: quotes, backslashes, newlines, tabs, language tags and datatypes are all its problem
/// and it is the same code that parses them back. Nothing here concatenates a raw string into
/// a literal, so there is no way for a title containing `" .` to end the statement.
fn insert_data(quads: &[Quad]) -> String {
    let mut by_graph: BTreeMap<Option<String>, Vec<&Quad>> = BTreeMap::new();
    for q in quads {
        let key = match &q.graph_name {
            GraphName::NamedNode(n) => Some(n.as_str().to_string()),
            // A blank node as a graph name is legal RDF and not something the registry mints;
            // it cannot be written in `INSERT DATA`'s `GRAPH` slot either, so it is grouped
            // with the default graph rather than silently forming a broken request.
            GraphName::BlankNode(_) | GraphName::DefaultGraph => None,
        };
        by_graph.entry(key).or_default().push(q);
    }
    let mut body = String::from("INSERT DATA {\n");
    for (graph, quads) in by_graph {
        let indent = if graph.is_some() { "    " } else { "  " };
        if let Some(g) = &graph {
            body.push_str(&format!("  GRAPH <{g}> {{\n"));
        }
        for q in quads {
            body.push_str(&format!("{indent}{} {} {} .\n", q.subject, q.predicate, q.object));
        }
        if graph.is_some() {
            body.push_str("  }\n");
        }
    }
    body.push_str("}\n");
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxigraph::model::{Literal, NamedNode, NamedOrBlankNode, Term};

    fn quad(o: Term) -> Quad {
        Quad::new(
            NamedNode::new_unchecked("https://reg.example/software/1"),
            NamedNode::new_unchecked("https://schema.org/name"),
            o,
            GraphName::NamedNode(NamedNode::new_unchecked("urn:tar:local")),
        )
    }

    #[test]
    fn a_literal_cannot_break_out_of_the_update_it_is_written_into() {
        let nasty = "he said \"hi\" \\ then\na newline\tand a tab } } INSERT DATA { <urn:evil> <urn:p> 1 } #";
        let body = insert_data(&[quad(Literal::new_simple_literal(nasty).into())]);
        // Four framing lines and one statement. The injected text is inside the quotes — it
        // did not open a second operation, add a triple or start a comment.
        assert_eq!(body.lines().count(), 5, "{body}");
        let statement = body.lines().nth(2).unwrap().trim();
        assert_eq!(statement.matches(" .").count(), 1, "{body}");

        // And it round-trips: the parser gives back exactly the string that went in, so the
        // escaping is not merely safe but lossless.
        let line = statement.replace(" .", " <urn:tar:local> .");
        let parsed: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
            .for_slice(line.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(parsed.len(), 1, "{line}");
        match &parsed[0].object {
            Term::Literal(l) => assert_eq!(l.value(), nasty),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn language_tags_and_datatypes_survive() {
        let body = insert_data(&[
            quad(Literal::new_language_tagged_literal("bonjour", "fr").unwrap().into()),
            quad(
                Literal::new_typed_literal("3", NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer"))
                    .into(),
            ),
        ]);
        assert!(body.contains("\"bonjour\"@fr"), "{body}");
        assert!(body.contains("\"3\"^^<http://www.w3.org/2001/XMLSchema#integer>"), "{body}");
    }

    #[test]
    fn quads_are_grouped_into_their_graphs_and_blank_nodes_are_kept() {
        let bnode = oxigraph::model::BlankNode::default();
        let quads = vec![
            quad(Literal::new_simple_literal("a").into()),
            Quad::new(
                NamedOrBlankNode::BlankNode(bnode.clone()),
                NamedNode::new_unchecked("http://spdx.org/rdf/terms#checksumValue"),
                Literal::new_simple_literal("deadbeef"),
                GraphName::NamedNode(NamedNode::new_unchecked("urn:tar:peer:other")),
            ),
        ];
        let body = insert_data(&quads);
        assert!(body.contains("GRAPH <urn:tar:local> {"), "{body}");
        assert!(body.contains("GRAPH <urn:tar:peer:other> {"), "{body}");
        assert!(body.contains(&format!("_:{}", bnode.as_str())), "{body}");
    }
}
