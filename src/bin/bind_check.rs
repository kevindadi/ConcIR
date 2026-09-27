//! Restricted binding-check CLI.
//!
//! Consumes the instrumenter's structural metadata (each resource's `display`
//! name, construction `site`, and for spawns the thread `entry` plus whether it
//! is unambiguous) and a CIR program, and emits per-resource verdicts:
//! `verified` / `unresolved` / `violated`. A versioned manifest may declare
//! bindings; each claim is checked, never trusted.
//!
//! Supported subset only: direct mutex/condvar/semaphore construction, channel
//! endpoints identified by a channel token, and spawns whose entry function is
//! unambiguous. Everything else is `unresolved`. This does not prove arbitrary
//! Rust or whole-program equivalence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::process;

use serde_json::{json, Value};

fn norm(s: &str) -> String {
    s.replace("preserved: ", "").trim().to_string()
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

fn binding_base(name: &str) -> String {
    // strip a trailing `_kindN` binding suffix
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

fn channel_token(name: &str) -> String {
    for suf in ["_tx", "_rx", "_sender", "_receiver", "tx", "rx"] {
        if let Some(stripped) = name.strip_suffix(suf) {
            let stripped = stripped.trim_end_matches(|c: char| c.is_ascii_digit());
            if !stripped.is_empty() {
                return stripped.to_string();
            }
        }
    }
    String::new()
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
        let d = r.get("display").and_then(Value::as_str)
            .unwrap_or_else(|| r.get("name").and_then(Value::as_str).unwrap_or(""));
        *display_counts.entry(d.to_string()).or_insert(0) += 1;
    }

    let mut verified: BTreeMap<String, Value> = BTreeMap::new();
    let mut unresolved: BTreeMap<String, Value> = BTreeMap::new();
    let mut used: BTreeSet<String> = BTreeSet::new();

    for r in resources {
        let kind = r.get("kind").and_then(Value::as_str).unwrap_or("");
        let name = r.get("name").and_then(Value::as_str).unwrap_or("");
        if kind == "Spawn" {
            continue;
        }
        if kind == "ChannelWrapper" {
            continue;
        }
        let display = r.get("display").and_then(Value::as_str).unwrap_or(name);
        if *display_counts.get(display).unwrap_or(&0) > 1 {
            unresolved.insert(name.to_string(), json!({
                "reason": "duplicate runtime resource name", "site": r.get("site")}));
            continue;
        }
        let short = binding_base(display);
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
            let token = channel_token(display);
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
            }
            unresolved.insert(name.to_string(), json!({
                "reason": "channel identity not established by a token",
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
    if let Some(list) = manifest.and_then(Value::as_array) {
        // A claim may key a resource by its runtime name or its display name.
        let mut by_display: BTreeMap<String, String> = BTreeMap::new();
        for (name, v) in &verified {
            if let Some(d) = resources.iter().find_map(|r| {
                (r.get("name").and_then(Value::as_str) == Some(name.as_str()))
                    .then(|| r.get("display").and_then(Value::as_str).unwrap_or("").to_string())
            }) {
                by_display.insert(d, name.clone());
            }
            let _ = v;
        }
        for claim in list {
            let rust = claim.get("rust").and_then(Value::as_str).unwrap_or("");
            let cir = claim.get("cir").and_then(Value::as_str).unwrap_or("");
            let key = if verified.contains_key(rust) {
                Some(rust.to_string())
            } else {
                by_display.get(rust).cloned()
            };
            match key.as_ref().and_then(|k| verified.get(k)) {
                Some(v) if v.get("cir").and_then(Value::as_str) == Some(cir) => {}
                other => {
                    violated.insert(rust.to_string(), json!({
                        "claim": cir,
                        "actual": other.and_then(|v| v.get("cir")),
                        "reason": "manifest disagrees with structure"}));
                }
            }
        }
    }
    // A claim cannot be both verified and violated.
    for k in violated.keys() {
        verified.remove(k);
    }
    json!({"verified": verified, "unresolved": unresolved, "violated": violated})
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut resources_path = None;
    let mut cir_path = None;
    let mut manifest_path: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--resources" => { resources_path = args.get(i + 1).cloned(); i += 2; }
            "--cir" => { cir_path = args.get(i + 1).cloned(); i += 2; }
            "--manifest" => { manifest_path = args.get(i + 1).cloned(); i += 2; }
            _ => { i += 1; }
        }
    }
    let (Some(rp), Some(cp)) = (resources_path, cir_path) else {
        eprintln!("usage: concir-bind-check --resources r.json --cir c.json [--manifest m.json]");
        process::exit(2);
    };
    let resources_doc: Value = serde_json::from_str(&fs::read_to_string(&rp).unwrap_or_default())
        .unwrap_or_else(|e| { eprintln!("resources parse error: {e}"); process::exit(2); });
    let resources = resources_doc.get("resources").and_then(Value::as_array).cloned().unwrap_or_default();
    let cir: Value = serde_json::from_str(&fs::read_to_string(&cp).unwrap_or_default())
        .unwrap_or_else(|e| { eprintln!("cir parse error: {e}"); process::exit(2); });
    let manifest: Option<Value> = manifest_path
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok());
    let result = check(&resources, &cir, manifest.as_ref());
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}
