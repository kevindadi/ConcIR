use std::collections::{HashSet, VecDeque};

use concir::ast::Program;
use concir::interp::Interpreter;
use concir::sem::program;
use concir::sem::system::TransitionSystem;

fn bfs_finished(src: &str, limit: usize) -> (usize, bool, usize) {
    let p: Program = serde_json::from_str(src).unwrap();
    let sp = program::lower(&p).unwrap();
    let it = Interpreter::new(&sp, Default::default());
    let init = it.initial().unwrap();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::new();
    seen.insert(init.clone());
    queue.push_back(init);
    let mut states = 0usize;
    let mut finished = false;
    while let Some(s) = queue.pop_front() {
        states += 1;
        if it.is_finished(&s) {
            finished = true;
        }
        if states > limit {
            break;
        }
        let en = it.successors(&s).unwrap();
        for step in en.steps {
            if seen.insert(step.state.clone()) {
                queue.push_back(step.state);
            }
        }
    }
    (states, finished, seen.len())
}

#[test]
fn producer_consumer_terminates() {
    let src = include_str!("../examples/producer_consumer.json");
    let (states, finished, seen) = bfs_finished(src, 20000);
    eprintln!("producer_consumer: states={states} seen={seen} finished={finished}");
    assert!(
        finished,
        "producer_consumer should have a terminating execution"
    );
}

#[test]
fn state_machine_runs() {
    let src = include_str!("../examples/state_machine.json");
    let (states, finished, seen) = bfs_finished(src, 20000);
    eprintln!("state_machine: states={states} seen={seen} finished={finished}");
    assert!(
        finished,
        "state_machine should have a terminating execution"
    );
}
