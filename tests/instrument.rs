//! Instrument v2: free Rust -> cir_trace::sync wrapper types.

use concir::instrument::wrap;

const SOURCE: &str = r#"use std::sync::{Arc, Mutex};
use std::thread;

fn main() {
    let mtx_a = Arc::new(Mutex::new(()));
    let mtx_b = Arc::new(Mutex::new(()));
    let (a1, b1) = (Arc::clone(&mtx_a), Arc::clone(&mtx_b));
    let w1 = thread::spawn(move || {
        let ga = a1.lock().unwrap();
        let gb = b1.lock().unwrap();
        drop(gb);
        drop(ga);
    });
    w1.join().unwrap();
}
"#;

#[test]
fn names_constructors_and_rewrites_imports() {
    let wrapped = wrap(SOURCE).expect("wrap");
    assert!(wrapped.annotated.contains("mod cir_trace;"));
    assert!(wrapped.annotated.contains("use cir_trace::sync::{Mutex, Condvar};"));
    assert!(!wrapped.annotated.contains("use std::sync::{Arc, Mutex}"));
    assert!(wrapped.annotated.contains(r#"Mutex::new_named("mtx_a_mutex0", ())"#));
    assert!(wrapped.annotated.contains(r#"Mutex::new_named("mtx_b_mutex0", ())"#));
    assert!(wrapped.annotated.contains(r#"cir_trace::spawn("w1", move ||"#));
    assert!(wrapped.annotated.contains("cir_trace::init();"));
    assert!(wrapped.annotated.contains("cir_trace::finish();"));

    let names: Vec<&str> = wrapped.resources.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"mtx_a_mutex0"));
    assert!(names.contains(&"mtx_b_mutex0"));
    assert!(names.contains(&"w1"));
    let spawn = wrapped.resources.iter().find(|r| r.name == "w1").unwrap();
    assert_eq!(spawn.kind, "Spawn");
}

#[test]
fn reports_uninstrumented_primitives() {
    let source = r#"use std::sync::RwLock;
fn main() { let _x = RwLock::new(0); }
"#;
    let wrapped = wrap(source).expect("wrap");
    assert!(wrapped.limitations.iter().any(|l| l.contains("RwLock")));
}

#[test]
fn condvar_named_without_arguments() {
    let source = r#"use std::sync::{Arc, Condvar, Mutex};
fn main() {
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let _ = pair;
}
"#;
    let wrapped = wrap(source).expect("wrap");
    assert!(wrapped.annotated.contains(r#"Mutex::new_named("pair_mutex0", false)"#));
    assert!(wrapped.annotated.contains(r#"Condvar::new_named("pair_condvar0")"#));
}

#[test]
fn nested_spawn_is_rewritten_once() {
    let source = r#"use std::sync::{Arc, Mutex};
use std::thread;
fn main() {
    let a = Arc::new(Mutex::new(()));
    let outer = thread::spawn(move || {
        let x1 = thread::spawn(move || { let _g = a.lock().unwrap(); });
        x1.join().unwrap();
    });
    outer.join().unwrap();
}
"#;
    let wrapped = wrap(source).expect("wrap");
    assert!(!wrapped.annotated.contains("spawnawn"));
    assert_eq!(wrapped.annotated.matches("cir_trace::spawn").count(), 2);
    assert!(wrapped.annotated.contains(r#"cir_trace::spawn("x1","#));
}

#[test]
fn module_local_mutex_import_is_preserved() {
    let source = r#"use std::sync::Arc;
mod module1 {
    use std::sync::{Arc, Mutex};
    pub struct ResourceA { pub lock: Mutex<()> }
    impl ResourceA {
        pub fn new() -> Arc<Self> {
            Arc::new(ResourceA { lock: Mutex::new(()) })
        }
    }
}
fn main() {}
"#;
    let wrapped = wrap(source).expect("wrap");
    assert!(wrapped.annotated.contains("use crate::cir_trace::sync::{Mutex};"));
    assert!(wrapped.annotated.contains("Mutex::new"));
}

#[test]
fn spawn_runtime_preserves_closure_output() {
    let source = r#"use std::sync::mpsc::sync_channel;
use std::thread;
fn main() {
    let (tx, rx) = sync_channel(0);
    let sender = thread::spawn(move || { tx.send(1).unwrap(); });
    let receiver = thread::spawn(move || rx.recv().unwrap());
    sender.join().unwrap();
    let done = receiver.join().unwrap();
    println!("DONE done={}", done);
}
"#;
    let wrapped = wrap(source).expect("wrap");
    assert!(wrapped.runtime.contains("fn spawn<F, T>"));
    assert!(wrapped.runtime.contains("F: FnOnce() -> T + Send + 'static"));
    assert!(wrapped.annotated.contains(r#"cir_trace::spawn("receiver""#));
}
