//! Content negotiation and FAIR Signposting (spec §4.4, §6.3).
//!
//! Every registry IRI dereferences as Turtle, JSON-LD, developer JSON or the HTML page, and
//! every resource GET carries Signposting `Link` headers so a machine client can navigate
//! without parsing the body.

use crate::error::{AppError, AppResult};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use oxigraph::io::{JsonLdProfile, RdfFormat, RdfSerializer};
use oxigraph::model::Quad;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repr {
    Json,
    Turtle,
    JsonLd,
    NQuads,
    Html,
    /// Markdown, for agents (`llms.txt` convention). The same record as the Turtle, written
    /// as prose an LLM reads without a parser and without the SPA's JavaScript.
    Markdown,
}

impl Repr {
    pub fn media_type(&self) -> &'static str {
        match self {
            Repr::Json => "application/json",
            Repr::Turtle => "text/turtle; charset=utf-8",
            Repr::JsonLd => "application/ld+json",
            Repr::NQuads => "application/n-quads",
            Repr::Html => "text/html; charset=utf-8",
            Repr::Markdown => "text/markdown; charset=utf-8",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Repr> {
        Some(match ext {
            "ttl" | "turtle" => Repr::Turtle,
            "jsonld" => Repr::JsonLd,
            "nq" | "nquads" => Repr::NQuads,
            "json" => Repr::Json,
            "html" => Repr::Html,
            "md" | "markdown" => Repr::Markdown,
            _ => return None,
        })
    }
}

/// Pick a representation from `Accept`, honouring q-values.
///
/// `default_repr` decides the tie: API routes default to JSON, IRI dereference routes default
/// to HTML for browsers but must still serve Turtle to `curl -H 'Accept: text/turtle'`.
pub fn negotiate(headers: &HeaderMap, default_repr: Repr) -> Repr {
    let Some(accept) = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) else {
        return default_repr;
    };
    let mut best: Option<(f32, Repr)> = None;
    for part in accept.split(',') {
        let mut bits = part.split(';');
        let media = bits.next().unwrap_or("").trim().to_ascii_lowercase();
        let mut q = 1.0f32;
        for p in bits {
            if let Some(v) = p.trim().strip_prefix("q=") {
                q = v.parse().unwrap_or(1.0);
            }
        }
        let repr = match media.as_str() {
            "text/turtle" | "application/x-turtle" | "text/n3" => Some(Repr::Turtle),
            "application/ld+json" => Some(Repr::JsonLd),
            "application/n-quads" | "application/n-triples" => Some(Repr::NQuads),
            "application/json" | "application/problem+json" => Some(Repr::Json),
            "text/html" | "application/xhtml+xml" => Some(Repr::Html),
            "text/markdown" | "text/x-markdown" => Some(Repr::Markdown),
            "*/*" => Some(default_repr),
            _ => None,
        };
        if let Some(r) = repr {
            if best.map(|(bq, _)| q > bq).unwrap_or(true) {
                best = Some((q, r));
            }
        }
    }
    best.map(|(_, r)| r).unwrap_or(default_repr)
}

pub fn serialize(quads: &[Quad], repr: Repr, base: &str) -> AppResult<String> {
    let format = match repr {
        Repr::Turtle => RdfFormat::Turtle,
        Repr::JsonLd => RdfFormat::JsonLd { profile: JsonLdProfile::Streaming.into() },
        Repr::NQuads => RdfFormat::NQuads,
        _ => RdfFormat::Turtle,
    };
    let mut ser = RdfSerializer::from_format(format);
    if matches!(repr, Repr::Turtle) {
        for (prefix, iri) in [
            ("tar", crate::ns::TAR),
            ("dcat", crate::ns::DCAT),
            ("dct", crate::ns::DCT),
            ("prov", crate::ns::PROV),
            ("schema", crate::ns::SCHEMA),
            ("rdfs", crate::ns::RDFS),
            ("skos", crate::ns::SKOS),
            ("spdx", crate::ns::SPDX),
            ("xsd", crate::ns::XSD),
        ] {
            ser = ser.with_prefix(prefix, iri).map_err(|e| AppError::internal(e.to_string()))?;
        }
        // Deliberately no `with_base_iri`: relativising against the base also relativises
        // the prefix IRIs, which produces Turtle that no other parser reads the same way.
        // A FAIR record is better off with absolute IRIs anyway.
        let _ = base;
    }
    let mut out = Vec::new();
    let mut w = ser.for_writer(&mut out);
    for q in quads {
        // Serialise as triples: the named-graph split is an internal provenance device, and
        // a client asking for a record wants the record.
        w.serialize_triple(q.as_ref()).map_err(|e| AppError::internal(e.to_string()))?;
    }
    w.finish().map_err(|e| AppError::internal(e.to_string()))?;
    String::from_utf8(out).map_err(|e| AppError::internal(e.to_string()))
}

/// Serialise quads *with* their graphs — used by `/admin/dump` and peer stub exchange.
pub fn serialize_quads(quads: &[Quad]) -> AppResult<String> {
    let mut out = Vec::new();
    let mut w = RdfSerializer::from_format(RdfFormat::NQuads).for_writer(&mut out);
    for q in quads {
        w.serialize_quad(q.as_ref()).map_err(|e| AppError::internal(e.to_string()))?;
    }
    w.finish().map_err(|e| AppError::internal(e.to_string()))?;
    String::from_utf8(out).map_err(|e| AppError::internal(e.to_string()))
}

/// FAIR Signposting link set (spec §6.3).
#[derive(Default)]
pub struct Signposting {
    links: Vec<String>,
}

impl Signposting {
    pub fn new(iri: &str) -> Self {
        let mut s = Self::default();
        s.links.push(format!("<{iri}>; rel=\"cite-as\""));
        s.links.push(format!("<{iri}.ttl>; rel=\"describedby\"; type=\"text/turtle\""));
        s.links.push(format!("<{iri}.jsonld>; rel=\"describedby\"; type=\"application/ld+json\""));
        // How an agent finds the prose rendering without knowing the convention in advance.
        s.links.push(format!("<{iri}.md>; rel=\"alternate\"; type=\"text/markdown\""));
        s
    }
    pub fn type_(mut self, iri: &str) -> Self {
        self.links.push(format!("<{iri}>; rel=\"type\""));
        self
    }
    /// `rel="item"` is emitted only for bytes that actually exist. `metadata-only` artifacts
    /// omit it, so a client can tell "no bytes here" from "bytes behind auth" (spec §6.3).
    pub fn item(mut self, url: &str, media_type: Option<&str>) -> Self {
        match media_type {
            Some(m) => self.links.push(format!("<{url}>; rel=\"item\"; type=\"{m}\"")),
            None => self.links.push(format!("<{url}>; rel=\"item\"")),
        }
        self
    }
    pub fn license(mut self, iri: &str) -> Self {
        self.links.push(format!("<{iri}>; rel=\"license\""));
        self
    }
    pub fn author(mut self, iri: &str) -> Self {
        self.links.push(format!("<{iri}>; rel=\"author\""));
        self
    }
    pub fn collection(mut self, iri: &str) -> Self {
        self.links.push(format!("<{iri}>; rel=\"collection\""));
        self
    }
    pub fn header_value(&self) -> Option<HeaderValue> {
        HeaderValue::from_str(&self.links.join(", ")).ok()
    }
}

/// A negotiated resource response: RDF or JSON, plus Signposting and `Vary: Accept`.
pub struct Negotiated {
    pub repr: Repr,
    pub body: String,
    pub signposting: Option<Signposting>,
    pub status: StatusCode,
}

impl Negotiated {
    pub fn json(value: &impl serde::Serialize) -> AppResult<Self> {
        Ok(Self {
            repr: Repr::Json,
            body: serde_json::to_string(value).map_err(|e| AppError::internal(e.to_string()))?,
            signposting: None,
            status: StatusCode::OK,
        })
    }
    pub fn with_signposting(mut self, s: Signposting) -> Self {
        self.signposting = Some(s);
        self
    }
    pub fn status(mut self, s: StatusCode) -> Self {
        self.status = s;
        self
    }
}

impl IntoResponse for Negotiated {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(match self.repr {
            Repr::Json => "application/json",
            Repr::Turtle => "text/turtle; charset=utf-8",
            Repr::JsonLd => "application/ld+json",
            Repr::NQuads => "application/n-quads",
            Repr::Html => "text/html; charset=utf-8",
            Repr::Markdown => "text/markdown; charset=utf-8",
        }));
        headers.insert(header::VARY, HeaderValue::from_static("Accept"));
        if let Some(v) = self.signposting.as_ref().and_then(|s| s.header_value()) {
            headers.insert(header::LINK, v);
        }
        (self.status, headers, self.body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT, HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn browsers_get_html_and_curl_gets_turtle() {
        let browser = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
        assert_eq!(negotiate(&accept(browser), Repr::Html), Repr::Html);
        assert_eq!(negotiate(&accept("text/turtle"), Repr::Html), Repr::Turtle);
        assert_eq!(negotiate(&accept("application/ld+json"), Repr::Html), Repr::JsonLd);
    }

    #[test]
    fn q_values_decide() {
        assert_eq!(negotiate(&accept("text/html;q=0.2, text/turtle;q=0.9"), Repr::Json), Repr::Turtle);
    }

    #[test]
    fn missing_accept_falls_back_to_the_route_default() {
        assert_eq!(negotiate(&HeaderMap::new(), Repr::Json), Repr::Json);
        assert_eq!(negotiate(&accept("*/*"), Repr::Html), Repr::Html);
    }
}
