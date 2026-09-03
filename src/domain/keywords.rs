//! The registry's own keyword list for artifacts.
//!
//! Free-text keywords are where a catalogue quietly stops being searchable: one deployment
//! writes `SHACL`, another `shacl`, a third `shacl shapes`, and a filter for any of them finds
//! a third of what is there. Subscriptions make that worse rather than better — a rule matching
//! on `keywords` silently misses deliveries it was written to catch.
//!
//! So the registry keeps a short controlled list. A keyword that matches one of these is
//! rewritten to its preferred label and additionally linked to the concept with `dcat:theme`;
//! anything else passes through untouched as `dcat:keyword` free text. That is DCAT's own
//! division — `dcat:theme` ranges over a `skos:Concept` from a scheme, `dcat:keyword` is a
//! literal — so nothing here is invented, and no existing record breaks.
//!
//! The list lives in code rather than in the graph because it is ours and it is small: making
//! it data would mean every write path needed a store lookup to normalise a handful of strings.

/// One entry in the list.
pub struct Keyword {
    /// The last segment of the concept IRI.
    pub slug: &'static str,
    /// What the UI shows and what a normalised keyword becomes.
    pub label: &'static str,
    pub definition: &'static str,
    /// Spellings that mean this keyword. Deliberately specific: an alias too broad folds
    /// unrelated records into a term, which is a worse failure than leaving them as free text.
    pub aliases: &'static [&'static str],
}

pub const KEYWORDS: &[Keyword] = &[
    Keyword {
        slug: "embeddings",
        label: "Embeddings",
        definition: "Dense vector representations of terms, documents or graph nodes.",
        aliases: &["embedding", "vector embeddings", "embedding vectors", "word embeddings"],
    },
    Keyword {
        slug: "owl",
        label: "OWL",
        definition: "An OWL ontology, or a module of one.",
        aliases: &["owl ontology", "owl 2", "owl2", "web ontology language"],
    },
    Keyword {
        slug: "rdf-graphs",
        label: "RDF Graphs",
        definition: "RDF data in any serialisation, named or default graph.",
        aliases: &["rdf graph", "rdf", "rdf data", "rdf dataset", "triples", "quads"],
    },
    Keyword {
        slug: "shacl",
        label: "SHACL",
        definition: "SHACL shapes, or the reports a SHACL processor produces from them.",
        aliases: &["shacl shapes", "shapes constraint language", "shacl shapes graph"],
    },
    Keyword {
        slug: "shex",
        label: "SHEX",
        definition: "ShEx schemas, or the results of validating against them.",
        aliases: &["shex", "shape expressions", "shex schema"],
    },
    Keyword {
        slug: "mappings",
        label: "Mappings",
        definition: "Declarative mappings from non-RDF sources into RDF.",
        aliases: &["mapping", "rml", "rml mapping", "r2rml", "yarrrml", "obda mapping"],
    },
    Keyword {
        slug: "sparql-query",
        label: "SPARQL query",
        definition: "A SPARQL query, or a stored set of them.",
        aliases: &["sparql", "sparql queries", "sparql query"],
    },
];

pub fn iri(base: &str, slug: &str) -> String {
    format!("{}/keyword/{}", base.trim_end_matches('/'), slug)
}

/// The scheme the whole list belongs to, so a consumer can find the others from any one of them.
pub fn scheme_iri(base: &str) -> String {
    format!("{}/scheme/artifact-keywords", base.trim_end_matches('/'))
}

/// Fold a string down to what two spellings of the same keyword have in common: case,
/// punctuation and spacing all stop mattering, so `SHACL shapes`, `shacl-shapes` and
/// `SHACL Shapes` are one key.
fn fold(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The list entry this text names, if any.
pub fn lookup(text: &str) -> Option<&'static Keyword> {
    let key = fold(text);
    if key.is_empty() {
        return None;
    }
    KEYWORDS.iter().find(|k| fold(k.label) == key || fold(k.slug) == key || k.aliases.iter().any(|a| fold(a) == key))
}

/// Rewrite a caller's keywords: the ones on the list become their preferred label and yield a
/// concept IRI, everything else is kept verbatim.
///
/// Order is preserved and duplicates are dropped, so `["shacl", "SHACL", "custom"]` becomes
/// `["SHACL", "custom"]` rather than repeating the same term under two spellings.
pub fn normalise(base: &str, keywords: &[String]) -> (Vec<String>, Vec<String>) {
    let mut labels: Vec<String> = Vec::new();
    let mut themes: Vec<String> = Vec::new();
    for raw in keywords {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        match lookup(trimmed) {
            Some(k) => {
                let theme = iri(base, k.slug);
                if !themes.contains(&theme) {
                    themes.push(theme);
                }
                if !labels.iter().any(|l| l == k.label) {
                    labels.push(k.label.to_string());
                }
            }
            None => {
                if !labels.iter().any(|l| l == trimmed) {
                    labels.push(trimmed.to_string());
                }
            }
        }
    }
    (labels, themes)
}

#[cfg(test)]
mod tests {
    use super::*;
    const BASE: &str = "https://reg.test";

    #[test]
    fn spellings_of_one_keyword_become_one_keyword() {
        let input: Vec<String> =
            ["shacl", "SHACL", "SHACL Shapes", "shacl-shapes"].iter().map(|s| s.to_string()).collect();
        let (labels, themes) = normalise(BASE, &input);
        assert_eq!(labels, vec!["SHACL"], "four spellings, one keyword");
        assert_eq!(themes, vec!["https://reg.test/keyword/shacl"]);
    }

    #[test]
    fn a_keyword_we_do_not_know_is_kept_rather_than_dropped() {
        // The list makes the common cases searchable; it must not silently discard what a
        // deployment knows about its own output.
        let input: Vec<String> = ["rdf", "pizza-ontology", "OWL"].iter().map(|s| s.to_string()).collect();
        let (labels, themes) = normalise(BASE, &input);
        assert_eq!(labels, vec!["RDF Graphs", "pizza-ontology", "OWL"]);
        assert_eq!(themes, vec!["https://reg.test/keyword/rdf-graphs", "https://reg.test/keyword/owl"]);
    }

    #[test]
    fn every_entry_is_reachable_by_its_own_label_and_slug() {
        for k in KEYWORDS {
            assert_eq!(lookup(k.label).map(|f| f.slug), Some(k.slug), "label {}", k.label);
            assert_eq!(lookup(k.slug).map(|f| f.slug), Some(k.slug), "slug {}", k.slug);
            for a in k.aliases {
                assert_eq!(lookup(a).map(|f| f.slug), Some(k.slug), "alias {a}");
            }
        }
    }

    #[test]
    fn no_alias_is_claimed_by_two_keywords() {
        // An alias landing on two entries would make normalisation depend on list order, which
        // is exactly the kind of quiet inconsistency this list exists to remove.
        let mut seen: Vec<(String, &str)> = Vec::new();
        for k in KEYWORDS {
            for text in std::iter::once(k.label).chain(std::iter::once(k.slug)).chain(k.aliases.iter().copied()) {
                let key = fold(text);
                if let Some((_, other)) = seen.iter().find(|(s, _)| *s == key) {
                    assert_eq!(*other, k.slug, "{text:?} is claimed by both {other} and {}", k.slug);
                }
                seen.push((key, k.slug));
            }
        }
    }

    #[test]
    fn blank_and_whitespace_keywords_are_dropped() {
        let input: Vec<String> = ["", "   ", "  SHACL  "].iter().map(|s| s.to_string()).collect();
        let (labels, _) = normalise(BASE, &input);
        assert_eq!(labels, vec!["SHACL"]);
    }
}
