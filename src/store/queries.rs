//! The portable SPARQL behind the reads that used to be bespoke store methods.
//!
//! `describe`, `exists`, `graph_of` and `count` were written against Oxigraph's quad-pattern
//! API, which meant their semantics lived inside one backend and a second backend would have
//! had to reimplement them from the doc comment. They are expressed here as SPARQL 1.1 text
//! instead, so the meaning is written down once and any store that speaks the standard
//! answers them the same way. `GraphStore` supplies them as default methods over `select` and
//! `ask`; no implementation overrides them.
//!
//! **Why `SELECT` and not `CONSTRUCT`.** A `CONSTRUCT` result is a set of *triples*. The graph
//! a quad came from is how `local` and `peer:…` origin is decided for every record and list
//! row (`GraphStore::graph_of`, `rdf::Props::graph`), so throwing it away is not an option.
//! `SELECT ?g ?s ?p ?o` is the same query with the graph kept, and it is what these builders
//! emit. `GraphStore::construct` remains for callers who genuinely want triples.

use crate::ns;
use anyhow::{anyhow, Result};
use oxigraph::model::NamedNode;

/// Predicates whose objects are sub-resources of the subject rather than records in their own
/// right (see `GraphStore::describe`). `dct:publisher` is deliberately absent: an Agent is its
/// own record, shared between many.
///
/// This list is the whole of the "owns outright" rule. Two bugs have come from a predicate
/// missing here — the triples exist, but the nested node stops coming back with its parent and
/// nothing reports an error — so it is the one constant in this file worth re-reading before
/// adding a nested node anywhere in `domain/`.
pub const OWNED_SUBRESOURCE_PREDICATES: [&str; 7] = [
    // An Attribution node says which agent played which role for *this* record. It is a
    // sub-resource in the same sense as a distribution: it exists only to qualify this subject
    // and is meaningless apart from it.
    "http://www.w3.org/ns/prov#qualifiedAttribution",
    "http://www.w3.org/ns/dcat#distribution",
    // An API description node carries dct:conformsTo and a title *about this record's API*.
    // Without this the triples exist but never come back with the record, because the node's
    // IRI is the document's own URL rather than something under the registry's base.
    "http://www.w3.org/ns/dcat#endpointDescription",
    "https://w3id.org/tar/ns#hasCapability",
    "https://w3id.org/tar/ns#sync",
    "http://spdx.org/rdf/terms#checksum",
    "http://www.w3.org/ns/prov#qualifiedAssociation",
];

pub fn is_owned_subresource(predicate: &str) -> bool {
    OWNED_SUBRESOURCE_PREDICATES.contains(&predicate)
}

/// Levels of ownership followed by the first attempt. The deepest chain the registry writes
/// today is subject → `dcat:distribution` → `spdx:checksum` (a blank node), which is level 2;
/// four leaves room for a nested node nobody has added yet without a second round trip. The
/// caller escalates rather than truncating — see [`truncated`].
pub const DEFAULT_DEPTH: usize = 4;
/// Where escalation gives up. A closure this deep is a cycle or a mistake, not a record.
pub const MAX_DEPTH: usize = 32;

/// `<iri>`, with the IRI validated first.
///
/// Every builder in this file goes through here. It keeps the old "bad IRI" error for a
/// caller that passes rubbish, and — because a `NamedNode` cannot contain `>`, a newline or a
/// space — it is also what stops a subject from closing the angle brackets and appending its
/// own query.
pub fn iri(value: &str) -> Result<String> {
    NamedNode::new(value).map_err(|e| anyhow!("bad IRI {value}: {e}"))?;
    Ok(format!("<{value}>"))
}

fn owned_list() -> String {
    OWNED_SUBRESOURCE_PREDICATES.iter().map(|p| format!("<{p}>")).collect::<Vec<_>>().join(", ")
}

/// The condition for following one edge: into a blank node whatever the predicate, or into a
/// named node when the predicate says the subject owns it outright.
fn follow(predicate_var: &str, object_var: &str) -> String {
    format!("FILTER(isBlank(?{object_var}) || ?{predicate_var} IN ({}))", owned_list())
}

/// The quads of `subject` together with the sub-resources it owns, to `depth` levels.
///
/// One request, not a walk. A client-side walk is impossible to write portably anyway: the
/// closure runs through blank nodes, and a blank node label from one result set cannot be put
/// back into the next query — `_:b0` in a query is a fresh existential, not a reference to the
/// store's node. Doing it in one query also keeps blank node labels consistent across the
/// whole result, which is what lets the Turtle serialiser render a checksum node inline.
///
/// Each `UNION` branch is one level of the closure and tags its rows with `?d`, so the caller
/// can see whether the deepest level found anything new and ask again with a longer chain.
/// Levels are joined across graphs, exactly as the quad-pattern walk was: a parent in
/// `<urn:tar:local>` whose distribution somehow sits in another graph still brings it back.
pub fn describe(subject: &str, depth: usize) -> Result<String> {
    let s = iri(subject)?;
    let mut branches = Vec::with_capacity(depth + 1);
    branches.push(format!("  {{ GRAPH ?g {{ {s} ?p ?o }} BIND({s} AS ?s) BIND(0 AS ?d) }}"));
    for level in 1..=depth {
        let mut chain = String::new();
        // Hop 1 starts at the subject; every later hop starts where the last one landed.
        for hop in 1..=level {
            let from = if hop == 1 { s.clone() } else { format!("?x{}", hop - 1) };
            chain.push_str(&format!(
                "    GRAPH ?g{hop} {{ {from} ?p{hop} ?x{hop} }} {}\n",
                follow(&format!("p{hop}"), &format!("x{hop}"))
            ));
        }
        branches.push(format!(
            "  {{\n{chain}    GRAPH ?g {{ ?x{level} ?p ?o }} BIND(?x{level} AS ?s) BIND({level} AS ?d)\n  }}"
        ));
    }
    Ok(format!("SELECT DISTINCT ?g ?s ?p ?o ?d WHERE {{\n{}\n}}", branches.join("\n  UNION\n")))
}

/// Whether a describe result may have been cut off at `depth`.
///
/// True only when the deepest level reached a node no shallower level did. A cycle re-visits
/// nodes already seen, so it answers false and escalation stops rather than running to
/// [`MAX_DEPTH`] on data that has nothing more to give.
pub fn truncated(levels: &[(usize, String)], depth: usize) -> bool {
    let shallower: std::collections::HashSet<&str> =
        levels.iter().filter(|(d, _)| *d < depth).map(|(_, s)| s.as_str()).collect();
    levels.iter().any(|(d, s)| *d == depth && !shallower.contains(s.as_str()))
}

/// Does any graph say anything about this subject, in subject position?
pub fn exists(subject: &str) -> Result<String> {
    Ok(format!("ASK {{ GRAPH ?g {{ {} ?p ?o }} }}", iri(subject)?))
}

/// Which named graph holds the authoritative statements about a subject.
///
/// The quad-pattern version returned whichever graph the storage index happened to yield
/// first, which is not a thing a second backend can reproduce and not a thing this one
/// promises across versions. The order is stated here instead, and it is the order every
/// caller wanted: what this registry holds itself outranks a cached copy of what a peer said,
/// and among peers the graph name breaks the tie so the answer is stable between calls.
pub fn graph_of(subject: &str) -> Result<String> {
    let s = iri(subject)?;
    Ok(format!(
        "SELECT ?g WHERE {{ GRAPH ?g {{ {s} ?p ?o }} }}\n\
         ORDER BY IF(?g = <{local}>, 0, IF(?g = <{vocab}>, 1, IF(?g = <{shapes}>, 2, 3))) STR(?g)\n\
         LIMIT 1",
        local = ns::G_LOCAL,
        vocab = ns::G_VOCAB,
        shapes = ns::G_SHAPES
    ))
}

/// Quads held, over every named graph.
///
/// The registry puts every quad in a named graph — `GraphTx` cannot express a default-graph
/// insert, `load_turtle` forces one, and the dumps `load_nquads` reads carry the names back —
/// so this is the whole store. It is also why `describe` and `exists` can ask for `GRAPH ?g`
/// and be sure they have missed nothing.
pub fn count() -> String {
    "SELECT (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }".to_string()
}

/// The `WHERE` body that matches a subject and everything it owns, within one graph.
///
/// This is `describe`'s closure again, narrowed to a single graph, for the deletion half of a
/// transaction: `GraphTx::replace_subject` removes a record *and its sub-resources*, or
/// replacing a record would orphan its distributions and checksums. Written as a pattern
/// rather than as a list of quads because the closure runs through blank nodes and
/// `DELETE DATA` may not name one.
pub fn owned_closure_body(subject: &str, graph: &str, depth: usize) -> Result<String> {
    let s = iri(subject)?;
    let g = iri(graph)?;
    let mut branches = Vec::with_capacity(depth + 1);
    branches.push(format!("    {{ GRAPH {g} {{ {s} ?p ?o }} BIND({s} AS ?s) }}"));
    for level in 1..=depth {
        let mut chain = String::new();
        for hop in 1..=level {
            let from = if hop == 1 { s.clone() } else { format!("?x{}", hop - 1) };
            chain.push_str(&format!(
                "      GRAPH {g} {{ {from} ?p{hop} ?x{hop} }} {}\n",
                follow(&format!("p{hop}"), &format!("x{hop}"))
            ));
        }
        branches.push(format!(
            "    {{\n{chain}      GRAPH {g} {{ ?x{level} ?p ?o }} BIND(?x{level} AS ?s)\n    }}"
        ));
    }
    Ok(branches.join("\n    UNION\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subject_cannot_close_the_angle_brackets_and_append_a_query() {
        // The one place a caller's string reaches query text. `NamedNode` refuses the
        // characters that would end the IRI reference, so there is nothing to escape.
        for bad in ["urn:x> } ; DROP ALL ; SELECT * WHERE { ?s ?p ?o . <urn:y", "a b", "x\ny"] {
            assert!(describe(bad, 2).is_err(), "{bad:?} must not reach the query");
            assert!(exists(bad).is_err(), "{bad:?} must not reach the query");
            assert!(graph_of(bad).is_err(), "{bad:?} must not reach the query");
        }
    }

    #[test]
    fn every_level_of_the_closure_is_reachable_from_the_subject() {
        let q = describe("https://reg.example/software/1", 3).unwrap();
        // Level 0 plus three chained levels.
        assert_eq!(q.matches("BIND(").count(), 8, "{q}");
        for level in 1..=3 {
            assert!(q.contains(&format!("BIND({level} AS ?d)")), "{q}");
        }
        // Every hop carries the ownership condition; none is a bare traversal.
        assert_eq!(q.matches("isBlank(").count(), 1 + 2 + 3, "{q}");
        assert!(q.contains("dcat#distribution"), "{q}");
        assert!(!q.contains("dc/terms/publisher"), "publisher is not owned: {q}");
    }

    #[test]
    fn truncation_is_reported_only_when_the_deepest_level_found_something_new() {
        let fresh = vec![(0, "a".into()), (1, "b".into()), (2, "c".into())];
        assert!(truncated(&fresh, 2));
        let cycle = vec![(0, "a".into()), (1, "b".into()), (2, "a".into())];
        assert!(!truncated(&cycle, 2));
        let shallow = vec![(0, "a".into()), (1, "b".into())];
        assert!(!truncated(&shallow, 2));
    }
}
