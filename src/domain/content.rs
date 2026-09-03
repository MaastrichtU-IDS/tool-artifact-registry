//! A name for bytes that two registries arrive at independently.
//!
//! Every record in this registry is identified by a UUIDv7 IRI minted by whichever registry the
//! record was created at. That is right for a *record* — it is this registry's statement, and
//! this registry is authoritative for it — and useless for the question federation actually
//! asks: two registries hold a description of the same file, and neither can tell. The IRIs are
//! unrelated, the titles are whatever two people typed, and the only thing genuinely shared —
//! the bytes — sits in a nested checksum node as a literal nobody can join on.
//!
//! So there is a second identity, derived rather than minted: a pure function of (algorithm,
//! digest) with no registry-specific input at all. Two registries handed the same digest
//! compute the same identifier, in the same way that two people running `sha256sum` compute the
//! same digest — no coordination, no lookup, no call to anybody.
//!
//! **The form: `ni:///sha-256;<base64url>`, RFC 6920.** What was weighed:
//!
//! * **RFC 6920 `ni:`** — chosen. It is the one IETF standards-track URI whose entire purpose is
//!   naming a thing by its digest; the algorithm is *inside* the name, so a sha-256 and a
//!   sha-512 of the same file cannot be confused for two names of different things, and the
//!   algorithm registry gives the digest length a normative source rather than a table invented
//!   here. It is a syntactically valid absolute URI, hence a legal RDF IRI and a legal object
//!   position, which is the constraint that actually binds.
//! * **`hash://sha256/<hex>`** (Preston, and the `content` CLI) — rejected, narrowly. Hex is
//!   what `sha256sum` prints, so a human can compare it to the identifier by eye, which is a
//!   real ergonomic win. But the scheme is a convention with no registration and no
//!   specification to point a peer at, and the ergonomic half is recoverable: this module
//!   accepts hex on input everywhere and reports it back beside the identifier.
//! * **W3C Hashlink** (`https://…/f.ttl?hl=zQm…`) — rejected as the wrong shape. A hashlink is a
//!   *URL with an integrity assertion attached*; it identifies a location and says what should be
//!   there. Two registries that hold the same bytes behind two different URLs get two different
//!   hashlinks, which is exactly the failure being fixed.
//! * **UUIDv5/v8 over the digest** — rejected. It would fit the existing IRI shape, and that is
//!   its only merit: it truncates 256 bits to 122, discards which algorithm produced them, needs
//!   a namespace UUID (a registry-specific input, the one thing forbidden here), and is not
//!   reversible — given the identifier you cannot recover the digest to check it against a file.
//! * **A multihash/CID** (`bafk…`) — rejected. Self-describing and elegant, but a CID for a file
//!   depends on the codec and the chunking, so two honest implementations disagree on the CID of
//!   one file; and it implies a content-addressed network to fetch from, which this registry is
//!   not.
//!
//! **The relation: `prov:specializationOf`, written on the distribution.** A `dcat:Distribution`
//! has a URL, a licence, a media type and an access protocol; the bit-string has none of those
//! and is the same bit-string wherever it is served from. PROV's word for that is specialization
//! — "shares all aspects of the latter, and additionally presents more specific aspects" — and
//! it is exactly the relation between "this file, at this URL, under this licence" and "the bytes
//! with this digest". What it beat:
//!
//! * **`owl:sameAs`** — too strong, and wrong in a way that would spread. It says the two IRIs
//!   denote one individual, so every statement about either is true of both. Two registries'
//!   records for one file are the same *content* and not the same *record*: different publisher,
//!   different licence claim, different distributions, different attribution. Asserting identity
//!   would let one registry's licence statement become a statement about the other's record.
//! * **`dct:identifier`** — refused on a concrete collision as well as on meaning. Its value is a
//!   literal, not a resource, so nothing joins on it; and this registry already writes the
//!   producer's own external key there, so a second one would make the two indistinguishable to
//!   every reader — `Props::str` would return whichever came back first.
//! * **`prov:alternateOf`** — nearly right, and rejected for being symmetric and transitive.
//!   Under it, an artifact's Turtle and N-Triples distributions would end up asserted as
//!   alternates of each other, which happens to be true; a dataset and its README, advertised as
//!   two distributions of one artifact, would too, which is false.
//! * **`dcat:`** — has nothing for this. DCAT puts the digest on the distribution as
//!   `spdx:checksum`, which is exactly the un-joinable literal this identifier exists to replace,
//!   and defines no property relating a distribution to a hash URI.
//!
//! **Why the distribution and not the artifact.** The digest is a fact about bytes, and bytes
//! are what a distribution has. An artifact is an abstract dataset that may have several
//! distributions with different digests, so a content identifier on the artifact would either
//! have to pick one arbitrarily or claim the artifact is several things at once. The artifact
//! stays findable through a one-hop path, which is how `?availability=` already works.

use crate::ns;
use crate::shacl::{Finding, Severity};
use base64::Engine;
use oxigraph::model::Quad;

/// The RFC 6920 `ni` scheme, as it appears at the head of every identifier this module derives.
const NI_PREFIX: &str = "ni:///";

/// A digest algorithm this registry recognises, and what it will do with one.
pub struct Algorithm {
    /// The name used in the identifier. For the deriving algorithms this is the RFC 6920
    /// registry's spelling, which is hyphenated where the everyday spelling is not.
    pub name: &'static str,
    /// Everything a producer might write in `checksum.algorithm` for it. Compared after
    /// lowercasing and stripping `-` and `_`, so the list only has to cover real spellings and
    /// not their punctuation variants.
    pub aliases: &'static [&'static str],
    /// Digest length in bytes. This is what makes a malformed digest detectable at all: a
    /// 63-character sha-256 is not a short sha-256, it is a typo that will never match anything.
    pub bytes: usize,
    /// Whether a content identifier is derived from it.
    ///
    /// Only the algorithms in the RFC 6920 registry are here, and that filter happens to do the
    /// security work as well: md5 and sha-1 have practical collisions, so an identifier built
    /// from one would let a caller construct two different files that claim a single identity.
    /// The digest is still recorded and still validated — the registry simply will not turn it
    /// into a name.
    pub derives: bool,
}

/// Known algorithms. Kept deliberately wider than the deriving set: a digest the registry cannot
/// name is still a digest worth checking the shape of, and a producer who computed a sha-1 has
/// not made a mistake, only a choice this registry cannot build an identity on.
pub const ALGORITHMS: &[Algorithm] = &[
    Algorithm { name: "sha-256", aliases: &["sha256", "sha2256"], bytes: 32, derives: true },
    Algorithm { name: "sha-384", aliases: &["sha384", "sha2384"], bytes: 48, derives: true },
    Algorithm { name: "sha-512", aliases: &["sha512", "sha2512"], bytes: 64, derives: true },
    Algorithm { name: "sha-224", aliases: &["sha224"], bytes: 28, derives: false },
    Algorithm { name: "sha3-256", aliases: &["sha3256"], bytes: 32, derives: false },
    Algorithm { name: "sha3-384", aliases: &["sha3384"], bytes: 48, derives: false },
    Algorithm { name: "sha3-512", aliases: &["sha3512"], bytes: 64, derives: false },
    Algorithm { name: "blake2b-256", aliases: &["blake2b256"], bytes: 32, derives: false },
    Algorithm { name: "blake2b-512", aliases: &["blake2b512"], bytes: 64, derives: false },
    Algorithm { name: "blake3", aliases: &["blake3"], bytes: 32, derives: false },
    Algorithm { name: "sha-1", aliases: &["sha1"], bytes: 20, derives: false },
    Algorithm { name: "md5", aliases: &["md5"], bytes: 16, derives: false },
];

/// The algorithms a caller can actually get an identifier out of, in the order they are offered.
pub fn deriving() -> Vec<&'static str> {
    ALGORITHMS.iter().filter(|a| a.derives).map(|a| a.name).collect()
}

fn normalise(algorithm: &str) -> String {
    algorithm.to_ascii_lowercase().chars().filter(|c| *c != '-' && *c != '_' && *c != ' ').collect()
}

pub fn algorithm(name: &str) -> Option<&'static Algorithm> {
    let n = normalise(name);
    ALGORITHMS.iter().find(|a| normalise(a.name) == n || a.aliases.contains(&n.as_str()))
}

/// What is wrong with a checksum, as a verdict separate from the wording.
///
/// Split the way `vocabulary::Verdict` is split, and for the same reason: the diagnosis has to be
/// identical whichever surface refused the write, while the advice differs between a REST client
/// and an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Not an algorithm this registry has a length for, so nothing about the digest can be
    /// checked and nothing can be derived from it.
    UnknownAlgorithm,
    /// A real algorithm, and a digest that is not one: wrong length, or not hex or base64.
    MalformedDigest { expected_hex_chars: usize },
    /// A real algorithm with a well-formed digest, from which this registry will not build a
    /// name. Never a violation — the write is fine, there is simply no identifier.
    NotDerivable,
}

impl Problem {
    /// One sentence saying what is wrong and what to do, naming the field the caller sent.
    pub fn message(&self, algorithm: &str) -> String {
        match self {
            Problem::UnknownAlgorithm => format!(
                "`{algorithm}` is not a digest algorithm this registry recognises, so it cannot \
                 check the digest is even the right length. Use one of: {}.",
                ALGORITHMS.iter().map(|a| a.name).collect::<Vec<_>>().join(", ")
            ),
            Problem::MalformedDigest { expected_hex_chars } => format!(
                "that is not a {algorithm} digest: it must be {expected_hex_chars} hexadecimal \
                 characters (what `sha256sum` and `openssl dgst` print), or the same digest in \
                 base64. A digest of the wrong length or alphabet cannot match anything, here or \
                 at any other registry."
            ),
            Problem::NotDerivable => format!(
                "a stable content identifier is not derived from {algorithm}; use one of: {}.",
                deriving().join(", ")
            ),
        }
    }
}

/// A content identifier and the equivalent spellings of the digest behind it.
///
/// Both encodings are carried because both are wanted by somebody: base64url is what goes in the
/// identifier, hex is what every command-line digest tool prints and therefore what a person
/// compares against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentId {
    pub iri: String,
    pub algorithm: &'static str,
    pub hex: String,
    pub base64url: String,
}

/// Decode a digest written as hex or as base64, to exactly `bytes` bytes.
///
/// The two encodings cannot be confused for one another at a fixed length — hex takes 2n
/// characters and base64 takes ⌈4n/3⌉ — so trying hex first costs nothing and never mis-reads a
/// base64 digest as a hex one.
fn decode_digest(value: &str, bytes: usize) -> Option<Vec<u8>> {
    let v = value.trim();
    if v.len() == bytes * 2 {
        if let Ok(raw) = hex::decode(v) {
            return Some(raw);
        }
    }
    // Producers copy digests out of JSON, out of shell output and out of other registries, so
    // both base64 alphabets and both padding conventions turn up. All four decode to the same
    // bytes, and the identifier normalises them into one spelling.
    let attempts = [
        base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(v),
        base64::engine::general_purpose::URL_SAFE.decode(v),
        base64::engine::general_purpose::STANDARD_NO_PAD.decode(v),
        base64::engine::general_purpose::STANDARD.decode(v),
    ];
    attempts.into_iter().flatten().find(|raw| raw.len() == bytes)
}

/// The whole feature, as one pure function: (algorithm, digest) in, identifier out.
///
/// Nothing here reads the store, the configuration or the base IRI, and that is the point rather
/// than an implementation detail — a peer that computes this from the same inputs must land on
/// the same string, and the only way to promise that is to give the function nothing else to
/// depend on.
pub fn identify(algorithm_name: &str, value: &str) -> Result<ContentId, Problem> {
    let Some(alg) = algorithm(algorithm_name) else { return Err(Problem::UnknownAlgorithm) };
    let Some(raw) = decode_digest(value, alg.bytes) else {
        return Err(Problem::MalformedDigest { expected_hex_chars: alg.bytes * 2 });
    };
    if !alg.derives {
        return Err(Problem::NotDerivable);
    }
    let base64url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw);
    Ok(ContentId {
        iri: format!("{NI_PREFIX}{};{base64url}", alg.name),
        algorithm: alg.name,
        hex: hex::encode(&raw),
        base64url,
    })
}

/// Check a digest without building an identifier, for the algorithms nothing is derived from.
///
/// A malformed sha-1 is still a typo worth refusing even though no name comes out of a sha-1.
pub fn check(algorithm_name: &str, value: &str) -> Result<(), Problem> {
    match identify(algorithm_name, value) {
        Ok(_) | Err(Problem::NotDerivable) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Accept whatever a caller has to hand as a content identifier and return the canonical form.
///
/// A filter that only understood the full `ni:///…` string would send a producer who has just run
/// `sha256sum` back to a conversion step, and the conversion is the registry's job. A bare digest
/// is unambiguous because the deriving algorithms have distinct lengths.
pub fn parse_query(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if let Some(rest) = v.strip_prefix(NI_PREFIX) {
        let (alg, digest) = rest.split_once(';')?;
        // Round-tripped rather than accepted verbatim: a caller who pastes the hex digest into
        // the `ni:` form, or leaves base64 padding on, still finds the record.
        return identify(alg, digest).ok().map(|c| c.iri);
    }
    ALGORITHMS.iter().filter(|a| a.derives).find_map(|a| identify(a.name, v).ok()).map(|c| c.iri)
}

// ------------------------------------------------------------------ write validation

/// A malformed digest as a validation finding, merged into the same `sh:ValidationReport` a shape
/// violation travels in.
///
/// **Why this is not a SHACL shape.** The constraint is "this literal must have the length and
/// alphabet that *that sibling literal* names", and SHACL Core has no way to say it: `sh:pattern`
/// is a fixed regex, and choosing the regex from another property's value needs `sh:sparql`,
/// which the engine here does not implement. Writing one shape per algorithm with
/// `sh:qualifiedValueShape` would express it, at a dozen shapes that have to be edited in step
/// with the table above — and would still produce a fixed `sh:message` unable to say which
/// algorithm was expected or how long the digest should have been.
///
/// It is a violation and not a warning, and it deliberately does not honour
/// `TAR_SHACL_VALIDATE_WRITES`, on the same reasoning as the vocabulary rule: a half-described
/// artifact is a trade an operator may make, but a digest that cannot be a digest is a false
/// claim about bytes, and it silently costs the record the one identity that would have let
/// another registry recognise it.
///
/// An algorithm this registry does not know is a *warning*: peer data and OpenLineage payloads
/// carry algorithms nobody here chose, and refusing them would reject correct records to protect
/// a field that is optional in the first place.
pub fn findings(quads: &[Quad]) -> Vec<Finding> {
    let fields = field_hints(quads);
    let mut out = Vec::new();
    for (subject, algorithm, value) in checksums_in(quads) {
        let problem = match check(&algorithm, &value) {
            Ok(()) => continue,
            Err(p) => p,
        };
        let severity = match problem {
            Problem::UnknownAlgorithm => Severity::Warning,
            _ => Severity::Violation,
        };
        out.push(Finding {
            severity,
            path: format!("{}checksumValue", ns::SPDX),
            // A real component: "the value does not match the required pattern" is what this is,
            // and a plain SHACL consumer that has never heard of this registry can read it.
            constraint: "PatternConstraintComponent".into(),
            message: problem.message(&algorithm),
            field: fields.get(&subject).cloned().unwrap_or_else(|| "distributions.checksum.value".into()),
            value: Some(value),
            focus: subject,
        });
    }
    out
}

/// Checksum node -> the input field that carried it, e.g. `distributions[1].checksum.value`.
///
/// A checksum sits on its own node one hop below the distribution, so `shacl::field_hints`, which
/// keys on the focus node of a shape violation, cannot reach it: the form would highlight nothing
/// and the caller would be told a digest is wrong without being told which one. The index is the
/// position of the distribution in the request, which is the order the quads were built in.
fn field_hints(quads: &[Quad]) -> std::collections::HashMap<String, String> {
    let distribution_p = format!("{}distribution", ns::DCAT);
    let checksum_p = format!("{}checksum", ns::SPDX);
    let mut index: std::collections::HashMap<String, usize> = Default::default();
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    for q in quads.iter().filter(|q| q.predicate.as_str() == distribution_p) {
        let parent = crate::rdf::props::subject_key(&q.subject);
        let n = seen.entry(parent).or_insert(0);
        index.insert(crate::rdf::props::term_key(&q.object), *n);
        *n += 1;
    }
    quads
        .iter()
        .filter(|q| q.predicate.as_str() == checksum_p)
        .filter_map(|q| {
            let i = index.get(&crate::rdf::props::subject_key(&q.subject))?;
            Some((crate::rdf::props::term_key(&q.object), format!("distributions[{i}].checksum.value")))
        })
        .collect()
}

/// Every (checksum node, algorithm, digest) a candidate write asserts.
///
/// Read out of the quads rather than off `DistributionIn` so that the rule covers every write
/// path at once — the REST create, both advertise endpoints, the OpenLineage adapter and the
/// seed all reach the store as quads, and a check bolted to one input type would miss the others.
fn checksums_in(quads: &[Quad]) -> Vec<(String, String, String)> {
    let algorithm_p = format!("{}algorithm", ns::SPDX);
    let value_p = format!("{}checksumValue", ns::SPDX);
    let mut algorithms: std::collections::HashMap<String, String> = Default::default();
    let mut values: std::collections::HashMap<String, String> = Default::default();
    for q in quads {
        let subject = crate::rdf::props::subject_key(&q.subject);
        if q.predicate.as_str() == algorithm_p {
            if let oxigraph::model::Term::NamedNode(n) = &q.object {
                let name = crate::ids::iri_tail(n.as_str()).trim_start_matches("checksumAlgorithm_");
                algorithms.insert(subject, name.to_string());
            }
        } else if q.predicate.as_str() == value_p {
            if let oxigraph::model::Term::Literal(l) = &q.object {
                values.insert(subject, l.value().to_string());
            }
        }
    }
    let mut out: Vec<(String, String, String)> =
        values.into_iter().filter_map(|(s, v)| algorithms.get(&s).map(|a| (s, a.clone(), v))).collect();
    // Blank node ids are not stable across runs, so a report built from an unordered map would
    // list the same two problems in a different order each time and no test could pin it.
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest of the empty string, which is the one sha-256 value that can be checked by eye
    /// against any other implementation.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn the_identifier_is_a_pure_function_of_algorithm_and_digest() {
        let a = identify("sha256", EMPTY_SHA256).expect("a real sha-256 digest");
        let b = identify("SHA-256", &EMPTY_SHA256.to_uppercase()).expect("the same digest, shouted");
        assert_eq!(a.iri, b.iri, "spelling of the algorithm or the case of the hex must not change the name");
        assert_eq!(
            a.iri, "ni:///sha-256;47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU",
            "the identifier must be the RFC 6920 form so another implementation lands on the same string"
        );
    }

    #[test]
    fn a_digest_given_in_base64_names_the_same_bytes_as_the_same_digest_in_hex() {
        let from_hex = identify("sha256", EMPTY_SHA256).unwrap();
        let from_b64 = identify("sha256", &from_hex.base64url).unwrap();
        let padded = identify("sha256", "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=").unwrap();
        assert_eq!(from_hex.iri, from_b64.iri, "hex and base64url of one digest are one identifier");
        assert_eq!(from_hex.iri, padded.iri, "the standard alphabet and padding must not fork the identity");
        assert_eq!(from_b64.hex, EMPTY_SHA256, "the hex form has to come back out for a person to compare");
    }

    #[test]
    fn a_digest_of_the_wrong_length_or_alphabet_is_refused_with_the_length_it_should_have() {
        for bad in ["e3b0c442", "not-a-digest", &EMPTY_SHA256[1..]] {
            match identify("sha256", bad) {
                Err(Problem::MalformedDigest { expected_hex_chars }) => assert_eq!(
                    expected_hex_chars, 64,
                    "the refusal must say how long a sha-256 is, or the caller cannot act on it"
                ),
                other => panic!("{bad} is not a sha-256 digest and must be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_algorithm_with_practical_collisions_yields_no_name_but_is_still_checked() {
        assert_eq!(
            identify("md5", "d41d8cd98f00b204e9800998ecf8427e"),
            Err(Problem::NotDerivable),
            "a name built on md5 could be claimed by two different files"
        );
        assert!(check("md5", "d41d8cd98f00b204e9800998ecf8427e").is_ok(), "a well-formed md5 is still a valid record");
        assert!(check("md5", "d41d8cd9").is_err(), "a malformed md5 is a typo whatever it is used for");
    }

    #[test]
    fn the_identifier_is_a_legal_iri_and_can_stand_in_an_object_position() {
        let id = identify("sha512", &"ab".repeat(64)).expect("a 64-byte digest");
        oxigraph::model::NamedNode::new(&id.iri)
            .expect("the identifier has to be a legal IRI or it cannot be written into the graph");
    }

    #[test]
    fn a_filter_accepts_the_identifier_or_the_bare_digest_a_producer_just_computed() {
        let canonical = identify("sha256", EMPTY_SHA256).unwrap().iri;
        assert_eq!(parse_query(EMPTY_SHA256).as_deref(), Some(canonical.as_str()), "hex out of sha256sum must work");
        assert_eq!(parse_query(&canonical).as_deref(), Some(canonical.as_str()), "so must the identifier itself");
        assert_eq!(
            parse_query(&format!("ni:///sha-256;{EMPTY_SHA256}")).as_deref(),
            Some(canonical.as_str()),
            "hex pasted into the identifier form is a near miss worth accepting"
        );
        assert_eq!(parse_query("nonsense").and_then(|_| Some(())), None, "a value that names no bytes matches nothing");
    }
}
