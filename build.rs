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
const VERSION_MARKER: &str = "# edam-version: ";
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60 * 24);

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={EDAM_TTL}");
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

    if current.as_deref() == Some(latest.as_str()) {
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
         # Topics classify software, data types classify artifacts. EDAM's format and operation\n\
         # branches are excluded, as is anything obsolete.\n\
         #\n\
         # ArtifactType is any IRI (spec D11): this bundle makes the common case pickable, it\n\
         # does not make EDAM mandatory.\n\n\
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
        out.push_str(&format!("<{iri}> a skos:Concept ;\n    skos:prefLabel \"{label}\" ;\n"));
        let definition = get(row, def_c);
        if let Some(first) = definition.split('|').next().filter(|d| !d.trim().is_empty()) {
            let d = escape(first);
            let d = if d.chars().count() > 400 { d.chars().take(400).collect() } else { d };
            out.push_str(&format!("    skos:definition \"{d}\" ;\n"));
        }
        for syn in get(row, syn_c).split('|').filter(|s| !s.trim().is_empty()).take(6) {
            out.push_str(&format!("    skos:altLabel \"{}\" ;\n", escape(syn)));
        }
        out.push_str(&format!("    tar:edamBranch \"{branch}\" .\n\n"));
        kept += 1;
    }
    if kept < 100 {
        return Err(format!("only {kept} usable terms parsed; refusing to write a broken bundle"));
    }
    Ok(out)
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
