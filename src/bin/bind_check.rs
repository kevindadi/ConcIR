//! Restricted binding-check CLI.
//!
//! Consumes the instrumenter's structural metadata (each resource's `display`
//! name, construction `site`, and for spawns the thread `entry` plus whether it
//! is unambiguous) and a CIR program, and emits per-resource verdicts:
//! `verified` / `unresolved` / `violated`. A versioned manifest may declare
//! bindings; each claim is checked, never trusted.
//!
//! Supported subset only: direct mutex/condvar/semaphore construction, channel
//! endpoints identified by an exact channel token, and spawns whose entry
//! function is unambiguous. Everything else is `unresolved`. This proves an
//! identity association only; it does NOT prove resource usage or whole-program
//! equivalence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::process;

use serde_json::{json, Value};

fn binding_base(name: &str) -> String {
    for kind in ["mutex", "condvar", "semaphore", "channel", "atomic", "var"] {
        if let Some(idx) = name.rfind(&format!("_{kind}")) {
            let tail = &name[idx + kind.len() + 1..];
            if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
                return name[..idx].to_string();
            }
        }
    }
    name.to_string()
}

/// Channel token = the endpoint name with its endpoint suffix removed. Digits
/// are part of the identity (`ch1_tx` -> `ch1`, never `ch`). An endpoint with no
/// channel token (`tx`, `rx`) returns the empty string (unresolved).
fn channel_token(name: &str) -> String {
    for suf in ["_sender", "_receiver", "_tx", "_rx"] {
        if let Some(stripped) = name.strip_suffix(suf) {
            return stripped.to_string();
        }
    }
    String::new()
}

fn cir_resources(cir: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(mods) = cir.get("modules").and_then(Value::as_array) {
        for m in mods {
            let mname = m.get("name").and_then(Value::as_str).unwrap_or("");
            if let Some(rs) = m.get("resources").and_then(Value::as_array) {
                for r in rs {
                    let rname = r.get("name").and_then(Value::as_str).unwrap_or("");
                    let ty = r.get("type").and_then(Value::as_str).unwrap_or("");
                    out.insert(format!("{mname}::{rname}"), ty.to_string());
                }
            }
        }
    }
    out
}

fn cir_threads(cir: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(mods) = cir.get("modules").and_then(Value::as_array) {
        for m in mods {
            let mname = m.get("name").and_then(Value::as_str).unwrap_or("");
            if let Some(fns) = m.get("functions").and_then(Value::as_array) {
                for f in fns {
                    let fname = f.get("name").and_then(Value::as_str).unwrap_or("");
                    if fname != "main" {
                        out.push(format!("{mname}::{fname}"));
                    }
                }
            }
        }
    }
    out
}

fn resource_display(r: &Value) -> String {
    r.get("display")
        .and_then(Value::as_str)
        .unwrap_or_else(|| r.get("name").and_then(Value::as_str).unwrap_or(""))
        .to_string()
}

fn check(resources: &[Value], cir: &Value, manifest: Option<&Value>) -> Value {
    let kinds = cir_resources(cir);
    let threads = cir_threads(cir);
    let mut by_short: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (fqn, kind) in &kinds {
        let short = fqn.rsplit("::").next().unwrap_or("").to_string();
        by_short.entry((short, kind.clone())).or_default().push(fqn.clone());
        by_kind.entry(kind.clone()).or_default().push(fqn.clone());
    }

    let mut display_counts: BTreeMap<String, usize> = BTreeMap::new();
    for r in resources {
        *display_counts.entry(resource_display(r)).or_insert(0) += 1;
    }

    // display -> runtime_id, for manifest alias resolution.
    let mut display_to_runtime: BTreeMap<String, String> = BTreeMap::new();
    for r in resources {
        let name = r.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        display_to_runtime.insert(resource_display(r), name);
    }

    let mut verified: BTreeMap<String, Value> = BTreeMap::new();
    let mut unresolved: BTreeMap<String, Value> = BTreeMap::new();
    let mut used: BTreeSet<String> = BTreeSet::new();

    for r in resources {
        let kind = r.get("kind").and_then(Value::as_str).unwrap_or("");
        let name = r.get("name").and_then(Value::as_str).unwrap_or("");
        if kind == "Spawn" || kind == "ChannelWrapper" {
            continue;
        }
        let display = resource_display(r);
        if *display_counts.get(&display).unwrap_or(&0) > 1 {
            unresolved.insert(name.to_string(), json!({
                "reason": "duplicate runtime resource name", "site": r.get("site")}));
            continue;
        }
        let short = binding_base(&display);
        let cands: Vec<String> = by_short
            .get(&(short.clone(), kind.to_string()))
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| !used.contains(c))
            .collect();
        if cands.len() == 1 {
            used.insert(cands[0].clone());
            verified.insert(name.to_string(), json!({"cir": cands[0], "rule": "exact"}));
            continue;
        }
        if kind == "Channel" {
            let token = channel_token(&display);
            if !token.is_empty() {
                let hits: Vec<String> = by_kind
                    .get("Channel")
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|f| f.rsplit("::").next() == Some(token.as_str()))
                    .collect();
                if hits.len() == 1 {
                    verified.insert(name.to_string(),
                                    json!({"cir": hits[0], "rule": "channel-name"}));
                    continue;
                }
                unresolved.insert(name.to_string(), json!({
                    "reason": "channel token does not match exactly one CIR channel",
                    "token": token, "site": r.get("site")}));
                continue;
            }
            unresolved.insert(name.to_string(), json!({
                "reason": "channel endpoint has no channel token",
                "site": r.get("site")}));
            continue;
        }
        unresolved.insert(name.to_string(), json!({
            "reason": "no structural evidence", "candidates": cands,
            "site": r.get("site")}));
    }

    for r in resources {
        if r.get("kind").and_then(Value::as_str) != Some("Spawn") {
            continue;
        }
        let name = r.get("name").and_then(Value::as_str).unwrap_or("");
        let entry = r.get("entry").and_then(Value::as_str);
        let unique = r.get("unique_entry").and_then(Value::as_bool).unwrap_or(false);
        if unique {
            if let Some(e) = entry {
                let hits: Vec<String> = threads
                    .iter()
                    .filter(|t| t.rsplit("::").next() == Some(e) && !used.contains(t.as_str()))
                    .cloned()
                    .collect();
                if hits.len() == 1 {
                    used.insert(hits[0].clone());
                    verified.insert(name.to_string(),
                                    json!({"cir": hits[0], "rule": "spawn-entry"}));
                    continue;
                }
            }
        }
        unresolved.insert(name.to_string(), json!({
            "reason": "thread entry is not unambiguous", "entry": entry,
            "site": r.get("site")}));
    }

    let mut violated: BTreeMap<String, Value> = BTreeMap::new();
    let mut claim_unresolved: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(list) = manifest.and_then(Value::as_array) {
        for claim in list {
            let rust = claim.get("rust").and_then(Value::as_str).unwrap_or("");
            let cir = claim.get("cir").and_then(Value::as_str).unwrap_or("");
            // Normalise the claim key (runtime id or display) to one object.
            let runtime = if verified.contains_key(rust) || unresolved.contains_key(rust) {
                Some(rust.to_string())
            } else {
                display_to_runtime.get(rust).cloned()
            };
            let Some(k) = runtime else {
                // The object/target does not exist: an input error, not a
                // proven structural contradiction.
                violated.insert(rust.to_string(), json!({
                    "claim": cir, "reason": "no such runtime resource or display name"}));
                continue;
            };
            let actual: Option<String> = verified
                .get(&k)
                .and_then(|v| v.get("cir").and_then(Value::as_str))
                .map(|s| s.to_string());
            if actual.as_deref() == Some(cir) {
                continue; // established relation agrees with the claim
            }
            if verified.contains_key(&k) {
                // An established relation conflicts with the claim.
                verified.remove(&k);
                unresolved.remove(&k);
                violated.insert(rust.to_string(), json!({
                    "claim": cir, "actual": actual, "runtime": k,
                    "reason": "manifest conflicts with an established binding"}));
            } else {
                // A valid object whose relation is not established: unresolved,
                // not a contradiction. Not promoted by relaxing name matching.
                unresolved.remove(&k);
                claim_unresolved.insert(rust.to_string(), json!({
                    "claim": cir, "runtime": k,
                    "reason": "no structural evidence to establish the declared relation"}));
            }
        }
    }
    unresolved.extend(claim_unresolved);

    json!({
        "verified": verified,
        "unresolved": unresolved,
        "violated": violated,
        "scope": "identity association only; not resource usage or whole-program equivalence",
    })
}

fn die(msg: &str) -> ! {
    eprintln!("bind-check input error: {msg}");
    process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut resources_path = None;
    let mut cir_path = None;
    let mut manifest_path: Option<String> = None;
    let mut source_sha: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--resources" => { resources_path = args.get(i + 1).cloned(); i += 2; }
            "--cir" => { cir_path = args.get(i + 1).cloned(); i += 2; }
            "--manifest" => { manifest_path = args.get(i + 1).cloned(); i += 2; }
            "--source-sha256" => { source_sha = args.get(i + 1).cloned(); i += 2; }
            _ => { i += 1; }
        }
    }
    let (Some(rp), Some(cp)) = (resources_path, cir_path) else {
        die("usage: concir-bind-check --resources r.json --cir c.json [--manifest m.json]");
    };
    let resources_doc: Value = match fs::read_to_string(&rp) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| die(&format!("resources parse error: {e}"))),
        Err(e) => die(&format!("cannot read resources '{rp}': {e}")),
    };
    let resources = resources_doc.get("resources").and_then(Value::as_array).cloned()
        .unwrap_or_else(|| die("resources.json has no 'resources' array"));
    let cir: Value = match fs::read_to_string(&cp) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| die(&format!("cir parse error: {e}"))),
        Err(e) => die(&format!("cannot read cir '{cp}': {e}")),
    };

    // Optional source fingerprint: the instrumenter records the source path it
    // read; verify it matches the expected hash.
    if let Some(expected) = source_sha {
        let source_path = resources_doc.get("source").and_then(Value::as_str).unwrap_or("");
        let actual = fs::read(source_path).ok().map(|b| sha256_hex(&b));
        if actual.as_deref() != Some(expected.as_str()) {
            die("source fingerprint mismatch");
        }
    }

    // An explicit manifest must be well-formed; a broken input is an error, not
    // "no manifest".
    let manifest: Option<Value> = match manifest_path {
        None => None,
        Some(p) => {
            let text = fs::read_to_string(&p)
                .unwrap_or_else(|e| die(&format!("cannot read manifest '{p}': {e}")));
            let value: Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| die(&format!("manifest parse error: {e}")));
            let arr = value.as_array().unwrap_or_else(|| die("manifest must be a JSON array"));
            for entry in arr {
                if entry.get("rust").and_then(Value::as_str).is_none()
                    || entry.get("cir").and_then(Value::as_str).is_none()
                {
                    die("manifest entry must have string 'rust' and 'cir'");
                }
            }
            Some(value)
        }
    };

    let result = check(&resources, &cir, manifest.as_ref());
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}

fn sha256_hex(bytes: &[u8]) -> String {
    concir::hash::sha256_hex(bytes)
}
