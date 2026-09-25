use concir::src_mutate::{append_comment, omit_notify_stmt, rename_local, swap_adjacent_locks};

#[test]
fn rename_does_not_double_apply_or_touch_fields() {
    let src = r#"fn w(m: i32) {
    let tmp2 = m;
    let value = tmp2 + 1;
    let _ = value;
}
struct S { tmp2: i32 }
"#;
    let once = rename_local(src, 0);
    assert!(once.applicable, "{}", once.reason);
    assert!(once.source.contains("let tmp2_kept"));
    assert!(once.source.contains("tmp2_kept + 1"));
    assert!(!once.source.contains("tmp2_kept_kept"));
    assert!(once.source.contains("tmp2: i32"), "field name must stay");
}

#[test]
fn swap_and_omit_keep_parseable_shapes() {
    let src = r#"fn main() {
    let _ga = a.lock().unwrap();
    let _gb = b.lock().unwrap();
    cv.notify_one();
}
"#;
    let swapped = swap_adjacent_locks(src, 0);
    assert!(swapped.applicable, "{}", swapped.reason);
    let ga = swapped.source.find("_ga").unwrap();
    let gb = swapped.source.find("_gb").unwrap();
    assert!(gb < ga);
    let omitted = omit_notify_stmt(src, 0);
    assert!(omitted.applicable);
    assert!(!omitted.source.contains("notify_one"));
    let comment = append_comment(src);
    assert!(comment.source.contains("// kept-comment"));
    assert!(comment.source.contains("notify_one"));
}
