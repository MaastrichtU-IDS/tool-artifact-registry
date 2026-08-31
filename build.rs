//! Build step: keep the bundled EDAM vocabulary current.
//!
//! The registry ships EDAM's topic and data branches (`shapes/edam.ttl`) so the term pickers
//! work with no network — the same promise the rest of the deployment makes. That bundle has to
//! come from somewhere, and pinning it by hand rots.
//!
//! So this checks the upstream release and regenerates when the bundle is missing or behind.
//! It is deliberately conservative about *when* it checks, because a build that hits the
//! network every time is slow, fails offline, and is not reproducible:
//!
//!   * the generated file is committed, so a normal checkout builds with no network at all;
//!   * the upstream check runs at most once a day, with a short timeout;
//!   * any failure — no curl, no network, a bad response — leaves the committed file alone and
//!     emits a warning rather than breaking the build;
//!   * `TAR_UPDATE_EDAM=1` forces a check, `TAR_EDAM_OFFLINE=1` skips it entirely.
//!
//! The only hard failure is having no bundle and no way to fetch one, which is a real problem
//! worth stopping for rather than papering over with an empty vocabulary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const EDAM_TTL: &str = "shapes/edam.ttl";
const ESV_TTL: &str = "shapes/euroscivoc.ttl";
const ESV_SCHEME: &str = "http://data.europa.eu/8mn/euroscivoc/40c0f173-baa3-48a3-9fe6-d6e8fb366a00";
const ESV_ENDPOINT: &str = "https://publications.europa.eu/webapi/rdf/sparql";
const ESV_MARKER: &str = "# euroscivoc-concepts: ";
const VERSION_MARKER: &str = "# edam-version: ";
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60 * 24);

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={EDAM_TTL}");
    println!("cargo:rerun-if-changed={ESV_TTL}");
    println!("cargo:rerun-if-env-changed=TAR_UPDATE_EDAM");
    println!("cargo:rerun-if-env-changed=TAR_EDAM_OFFLINE");

    let ttl = PathBuf::from(EDAM_TTL);
    let have = ttl.exists();
    let forced = std::env::var("TAR_UPDATE_EDAM").is_ok_and(|v| v == "1");
    let offline = std::env::var("TAR_EDAM_OFFLINE").is_ok_and(|v| v == "1");

    if offline {
        if !have {
            panic!("TAR_EDAM_OFFLINE=1 but {EDAM_TTL} does not exist — nothing to build the term pickers from");
        }
        return;
    }
    // EuroSciVoc classifies the software; EDAM's data branch types the artifacts. Two
    // vocabularies because they answer different questions, and one that answers both badly
    // is what the topic list looked like before.
    update_euroscivoc(forced);
    if have && !forced && !due_for_check() {
        return;
    }

    let current = have.then(|| read_version(&ttl)).flatten();
    let latest = match latest_release() {
        Some(v) => v,
        None => {
            if have {
                warn("could not reach the EDAM release feed; keeping the bundled vocabulary");
                return;
            }
            panic!(
                "{EDAM_TTL} is missing and the EDAM release feed is unreachable.\n\
                 Restore the file from version control, or build once with network access."
            );
        }
    };
    mark_checked();

    // `forced` regenerates even when the version matches: the generator's own output format
    // changes too, and a force that only re-checked the version could never apply one.
    if current.as_deref() == Some(latest.as_str()) && !forced {
        return;
    }
    match current {
        Some(c) => println!("cargo:warning=EDAM {c} is behind {latest}; regenerating {EDAM_TTL}"),
        None => println!("cargo:warning=generating {EDAM_TTL} from EDAM {latest}"),
    }

    match fetch_csv(&latest) {
        Some(csv) => match generate(&csv, &latest) {
            Ok(turtle) => {
                fs::write(&ttl, turtle).expect("writing the EDAM bundle");
                println!("cargo:warning=EDAM bundle updated to {latest}");
            }
            Err(e) => fail_or_warn(have, &format!("could not parse the EDAM release: {e}")),
        },
        None => fail_or_warn(have, "could not download the EDAM release"),
    }
}

fn fail_or_warn(have_existing: bool, message: &str) {
    if have_existing {
        warn(&format!("{message}; keeping the bundled vocabulary"));
    } else {
        panic!("{message}, and there is no bundled vocabulary to fall back on");
    }
}

fn warn(message: &str) {
    println!("cargo:warning={message}");
}

/// The newest tag on the EDAM repository, e.g. `1.25.20260626T1230Z`.
fn latest_release() -> Option<String> {
    let out = curl("https://api.github.com/repos/edamontology/edamontology/releases/latest")?;
    // A whole JSON parser for one field is not worth a build dependency.
    let key = "\"tag_name\":";
    let start = out.find(key)? + key.len();
    let rest = out[start..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn fetch_csv(version: &str) -> Option<String> {
    // The tagged asset is the reproducible one; the unversioned URL follows whatever is current.
    let tagged = format!(
        "https://raw.githubusercontent.com/edamontology/edamontology/{version}/EDAM_dev.csv"
    );
    curl(&tagged).or_else(|| curl("https://edamontology.org/EDAM.csv"))
}

fn curl(url: &str) -> Option<String> {
    let out = Command::new("curl")
        .args(["-sfL", "--max-time", "60", "-H", "User-Agent: tool-artifact-registry-build", url])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn read_version(path: &Path) -> Option<String> {
    let head: String = fs::read_to_string(path).ok()?.lines().take(20).collect::<Vec<_>>().join("\n");
    head.lines()
        .find_map(|l| l.strip_prefix(VERSION_MARKER))
        .map(|v| v.trim().to_string())
}

fn stamp() -> PathBuf {
    PathBuf::from(std::env::var("OUT_DIR").unwrap_or_else(|_| ".".into())).join("edam-checked")
}

fn due_for_check() -> bool {
    match fs::metadata(stamp()).and_then(|m| m.modified()) {
        Ok(t) => SystemTime::now().duration_since(t).map(|age| age > CHECK_INTERVAL).unwrap_or(true),
        Err(_) => true,
    }
}

fn mark_checked() {
    let _ = fs::write(stamp(), "");
}

/// Turn the EDAM CSV into the Turtle the registry loads.
///
/// Topics classify software and data types classify artifacts; EDAM's format and operation
/// branches are not what this registry types things with, and obsolete terms would offer the
/// picker names nobody should choose.
fn generate(csv_text: &str, version: &str) -> Result<String, String> {
    let mut rows = parse_csv(csv_text);
    let header = if rows.is_empty() { return Err("empty CSV".into()) } else { rows.remove(0) };
    let col = |name: &str| header.iter().position(|h| h == name);
    let (Some(id_c), Some(label_c)) = (col("Class ID"), col("Preferred Label")) else {
        return Err("CSV is missing the Class ID or Preferred Label column".into());
    };
    let obsolete_c = col("Obsolete");
    let def_c = col("Definitions");
    let syn_c = col("Synonyms");

    let mut out = String::new();
    out.push_str(&format!("{VERSION_MARKER}{version}\n"));
    out.push_str(
        "# EDAM topics and data types, bundled so the registry's term pickers work offline.\n\
         #\n\
         # GENERATED by build.rs from the EDAM release named above — do not edit by hand.\n\
         # The data branch types artifacts, so it is emitted as tar:ArtifactType. The topic\n\
         # branch is kept only so records that already cite an EDAM topic still render a label —\n\
         # software topics come from EuroSciVoc now, because asked to classify this estate EDAM\n\
         # returned the same two generic topics for every tool in it — so it is emitted as\n\
         # tar:LegacyTopic, which no picker offers and no write accepts.\n\
         #\n\
         # The class is written into the same statement as `a skos:Concept` on purpose: the kind\n\
         # used to be a separate tar:conceptBranch triple, and a marker that can be written\n\
         # somewhere else than the concept eventually is.\n\n\
         @prefix skos: <http://www.w3.org/2004/02/skos/core#> .\n\
         @prefix tar:  <https://w3id.org/tar/ns#> .\n\n",
    );

    let get = |row: &Vec<String>, i: Option<usize>| i.and_then(|i| row.get(i)).cloned().unwrap_or_default();
    let mut kept = 0usize;
    for row in &rows {
        let iri = get(row, Some(id_c));
        let Some(local) = iri.strip_prefix("http://edamontology.org/") else { continue };
        let branch = local.split('_').next().unwrap_or_default();
        if branch != "topic" && branch != "data" {
            continue;
        }
        if get(row, obsolete_c).eq_ignore_ascii_case("true") {
            continue;
        }
        let label = escape(&get(row, Some(label_c)));
        if label.is_empty() {
            continue;
        }
        // EDAM topics stay bundled so that any record already citing one — ours or a peer's —
        // still renders a label, but they are typed away from the class the topic picker offers,
        // which is EuroSciVoc's now.
        let class = if branch == "topic" { "tar:LegacyTopic" } else { "tar:ArtifactType" };
        out.push_str(&format!("<{iri}> a skos:Concept, {class} ;\n    skos:prefLabel \"{label}\" ;\n"));
        let definition = get(row, def_c);
        if let Some(first) = definition.split('|').next().filter(|d| !d.trim().is_empty()) {
            let d = escape(first);
            let d = if d.chars().count() > 400 { d.chars().take(400).collect() } else { d };
            out.push_str(&format!("    skos:definition \"{d}\" ;\n"));
        }
        for syn in get(row, syn_c).split('|').filter(|s| !s.trim().is_empty()).take(6) {
            out.push_str(&format!("    skos:altLabel \"{}\" ;\n", escape(syn)));
        }
        close_block(&mut out);
        kept += 1;
    }
    if kept < 100 {
        return Err(format!("only {kept} usable terms parsed; refusing to write a broken bundle"));
    }
    Ok(out)
}

/// Finish a concept's statement block: whichever property turned out to be last, it ends with a
/// full stop rather than a semicolon. Every property below the type line is optional, so which
/// one that is varies per term.
fn close_block(out: &mut String) {
    if out.ends_with(";\n") {
        out.truncate(out.len() - 2);
        out.push_str(".\n");
    }
    out.push('\n');
}

fn escape(s: &str) -> String {
    s.trim().replace('\\', "\\\\").replace('"', "\\\"").replace(['\n', '\r'], " ")
}

/// RFC 4180 enough for this file: quoted fields, doubled quotes, embedded commas and newlines.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') => {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            }
            (true, _) => field.push(c),
            (false, '"') => quoted = true,
            (false, ',') => row.push(std::mem::take(&mut field)),
            (false, '\n') => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            (false, '\r') => {}
            (false, _) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}


// ---------------------------------------------------------------- EuroSciVoc

/// Refresh the bundled EU Science Vocabulary, used for the topics on a Software record.
///
/// EuroSciVoc has no release tag to compare against the way EDAM does, so this refetches on
/// the same daily cadence and rewrites only when the content actually differs. Same failure
/// posture as EDAM: a checkout builds offline from the committed file, and an unreachable
/// endpoint warns rather than breaking the build.
fn update_euroscivoc(forced: bool) {
    let ttl = PathBuf::from(ESV_TTL);
    let have = ttl.exists();
    if have && !forced && !due_for_check_named("esv-checked") {
        return;
    }
    let Some(csv) = fetch_euroscivoc() else {
        if have {
            warn("could not reach the EU Publications Office endpoint; keeping the bundled EuroSciVoc");
        } else {
            panic!(
                "{ESV_TTL} is missing and the EU SPARQL endpoint is unreachable.\n\
                 Restore the file from version control, or build once with network access."
            );
        }
        return;
    };
    mark_checked_named("esv-checked");
    match generate_euroscivoc(&csv) {
        Ok(turtle) => {
            let unchanged = have
                && fs::read_to_string(&ttl)
                    .map(|old| strip_header(&old) == strip_header(&turtle))
                    .unwrap_or(false);
            if unchanged {
                return;
            }
            fs::write(&ttl, turtle).expect("writing the EuroSciVoc bundle");
            println!("cargo:warning=EuroSciVoc bundle regenerated");
        }
        Err(e) => {
            if have {
                warn(&format!("{e}; keeping the bundled EuroSciVoc"));
            } else {
                panic!("{e}, and there is no bundled EuroSciVoc to fall back on");
            }
        }
    }
}

/// Compare content, not the generation stamp, so an unchanged vocabulary does not churn the file.
fn strip_header(ttl: &str) -> String {
    ttl.lines().filter(|l| !l.starts_with("# generated:")).collect::<Vec<_>>().join("\n")
}

fn fetch_euroscivoc() -> Option<String> {
    let query = format!(
        "PREFIX skos: <http://www.w3.org/2004/02/skos/core#>\n\
         SELECT ?c ?l ?d ?bl WHERE {{\n\
           ?c skos:inScheme <{ESV_SCHEME}> ; skos:prefLabel ?l . FILTER(LANG(?l)='en')\n\
           OPTIONAL {{ ?c skos:definition ?d . FILTER(LANG(?d)='en') }}\n\
           OPTIONAL {{ ?c skos:broader ?b . ?b skos:prefLabel ?bl . FILTER(LANG(?bl)='en') }}\n\
         }}"
    );
    let out = Command::new("curl")
        .args([
            "-sfL", "--max-time", "120", "-G", ESV_ENDPOINT,
            "--data-urlencode", &format!("query={query}"),
            "--data-urlencode", "format=text/csv",
            "-H", "User-Agent: tool-artifact-registry-build",
        ])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn generate_euroscivoc(csv_text: &str) -> Result<String, String> {
    let mut rows = parse_csv(csv_text);
    let header = if rows.is_empty() { return Err("empty EuroSciVoc response".into()) } else { rows.remove(0) };
    let col = |name: &str| header.iter().position(|h| h == name);
    let (Some(c), Some(l)) = (col("c"), col("l")) else {
        return Err("EuroSciVoc response is missing the ?c or ?l column".into())
    };
    let (d, bl) = (col("d"), col("bl"));

    let mut out = String::new();
    let mut body = String::new();
    let mut kept = 0usize;
    for row in &rows {
        let get = |i: Option<usize>| i.and_then(|i| row.get(i)).cloned().unwrap_or_default();
        let iri = row.get(c).cloned().unwrap_or_default();
        if !iri.starts_with("http://data.europa.eu/") {
            continue;
        }
        let label = escape(&row.get(l).cloned().unwrap_or_default());
        if label.is_empty() {
            continue;
        }
        body.push_str(&format!("<{iri}> a skos:Concept, tar:ResearchTopic ;\n    skos:prefLabel \"{label}\" ;\n"));
        let definition = escape(&get(d));
        if !definition.is_empty() {
            body.push_str(&format!("    skos:definition \"{definition}\" ;\n"));
        }
        // EuroSciVoc carries almost no definitions but nearly always a parent, and "ontology,
        // in knowledge engineering" is the disambiguation a picker actually needs — there is
        // also odontology and palaeontology in here.
        let broader = escape(&get(bl));
        if !broader.is_empty() {
            body.push_str(&format!("    tar:inBroader \"{broader}\" ;\n"));
        }
        close_block(&mut body);
        kept += 1;
    }
    if kept < 100 {
        return Err(format!("only {kept} EuroSciVoc concepts parsed; refusing to write a broken bundle"));
    }
    out.push_str(&format!("{ESV_MARKER}{kept}\n"));
    out.push_str(&format!("# generated: {}\n", now_date()));
    out.push_str(
        "# EU Science Vocabulary (EuroSciVoc) — the topics a Software record is classified by.\n\
         #\n\
         # GENERATED by build.rs from the EU Publications Office SPARQL endpoint — do not edit.\n\
         # Scheme: http://data.europa.eu/8mn/euroscivoc/\n\
         #\n\
         # Why this and not EDAM's topic branch: EDAM is a life-science vocabulary, and asked to\n\
         # classify a SHACL validator, an ontology browser, a schema builder and an RDF mapper it\n\
         # returned the same two generic topics for all four. EuroSciVoc has `semantic web`,\n\
         # `ontology` (under knowledge engineering), `databases` and `software`, and DCAT-AP uses\n\
         # it for dct:subject — so a harvester understands these records already.\n\
         #\n\
         # Emitted as tar:ResearchTopic, in the same statement as `a skos:Concept`: the kind used\n\
         # to be a separate tar:conceptBranch triple, and a marker that can be written somewhere\n\
         # else than the concept eventually is.\n\n\
         @prefix skos: <http://www.w3.org/2004/02/skos/core#> .\n\
         @prefix tar:  <https://w3id.org/tar/ns#> .\n\n",
    );
    out.push_str(&body);
    Ok(out)
}

fn now_date() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn due_for_check_named(name: &str) -> bool {
    let path = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_else(|_| ".".into())).join(name);
    match fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => SystemTime::now().duration_since(t).map(|age| age > CHECK_INTERVAL).unwrap_or(true),
        Err(_) => true,
    }
}

fn mark_checked_named(name: &str) {
    let path = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_else(|_| ".".into())).join(name);
    let _ = fs::write(path, "");
}
