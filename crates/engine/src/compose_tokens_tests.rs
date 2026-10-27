//! OneOS Pages & apps: the token-constrained mode (`compose_tokens.rs`) and `bind.field`.

use crate::Session;
use serde_json::{Value, json};

fn tokens() -> Value {
    json!({
        "spacing": {"none": 0, "sm": 8, "md": 16, "lg": 24, "xl": 32},
        "radii": {"sm": 8, "md": 12},
        "type": {"body": 16, "h3": 24, "h1": 40},
        "colors": {"surface": "#F5F5F7", "ink": "#111111", "white": "#FFFFFF", "accent": "#3D5AFE"}
    })
}

fn run(s: &mut Session, steps: Value, extra: Value) -> Result<Value, String> {
    let mut p = json!({"label": "t", "steps": steps});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        p[k] = v;
    }
    s.run_command_batch(&p)
}

/// A compose page frame with one auto-layout section made by the host (trusted), tokens on.
fn page(s: &mut Session) -> u64 {
    s.set_compose_tokens(Some(&tokens())).unwrap();
    let r = run(
        s,
        json!([
            {"name": "node.create", "args": {"type": "frame", "name": "Page", "select": false, "x": 0, "y": 0, "width": 400, "height": 400,
              "props": {"layout": "vertical", "gap": 16, "padding": 16, "fill": "#FFFFFF", "pluginData": {"oneos.page": "home"}}}},
            {"name": "node.create", "args": {"type": "text", "name": "t", "select": false, "parent": {"$step": 0}, "text": "Hi",
              "props": {"fontSize": 16, "fill": "#111111", "pluginData": {"oneos.id": "t"}}}}
        ]),
        json!({"trusted": true}),
    )
    .expect("the host draws tokens only");
    r["steps"][0]["result"]["id"].as_u64().unwrap()
}

#[test]
fn a_raw_value_on_a_page_is_refused_by_name_and_nothing_lands() {
    let mut s = Session::new();
    let f = page(&mut s);
    let before = s.undo_depth().0;
    let gap = run(&mut s, json!([{"name": "node.set", "args": {"id": f, "props": {"gap": 20}}}]), json!({})).expect_err("gap 20");
    assert!(gap.starts_with("token-refused") && gap.contains("gap 20") && gap.contains("md"), "{gap}");
    let col = run(&mut s, json!([{"name": "node.set", "args": {"id": f, "props": {"fill": "#FF00AA"}}}]), json!({})).expect_err("raw colour");
    assert!(col.contains("#FF00AA") && col.contains("theme colour role"), "{col}");
    let ty = run(
        &mut s,
        json!([{"name": "node.create", "args": {"type": "text", "parent": f, "text": "x", "props": {"fontSize": 19, "fill": "#111111"}}}]),
        json!({}),
    )
    .expect_err("type 19");
    assert!(ty.contains("font size 19") && ty.contains("type token"), "{ty}");
    assert_eq!(s.undo_depth().0, before, "nothing landed");
    // Tokens pass.
    run(&mut s, json!([{"name": "node.set", "args": {"id": f, "props": {"gap": 24, "fill": "#F5F5F7"}}}]), json!({})).expect("tokens");
}

#[test]
fn the_ui_path_is_refused_the_same_way() {
    let mut s = Session::new();
    let f = page(&mut s);
    let e = s.execute("node.set", &json!({"id": f, "props": {"padding": 13}})).expect_err("raw padding by the panel");
    assert!(e.to_string().contains("padding 13"), "{e}");
}

#[test]
fn the_sidecar_is_the_hosts_and_bind_field_is_typed() {
    let mut s = Session::new();
    let f = page(&mut s);
    let e = run(&mut s, json!([{"name": "node.set", "args": {"id": f, "props": {"pluginData": {"oneos.c": "section"}}}}]), json!({}))
        .expect_err("free sidecar write");
    assert!(e.contains("oneos.* sidecar"), "{e}");
    run(&mut s, json!([{"name": "node.set", "args": {"id": f, "props": {"pluginData": {"oneos.c": "section"}}}}]), json!({"trusted": true}))
        .expect("the host");
    let kid = s.doc().unwrap().node(crate::NodeId(f)).unwrap().children[0].0;
    let ok = run(&mut s, json!([{"name": "bind.field", "args": {"id": kid, "prop": "text", "field": "title", "scope": "record"}}]), json!({}))
        .expect("bind");
    assert_eq!(ok["steps"][0]["result"]["field"], "title");
    let stored = s.doc().unwrap().node(crate::NodeId(kid)).unwrap().plugin_data.get("oneos.bindField").cloned().unwrap();
    assert_eq!(serde_json::from_str::<Value>(&stored).unwrap(), json!([{"prop": "text", "field": "title", "scope": "record"}]));
    for bad in [
        json!({"id": kid, "prop": "fill", "field": "x"}),
        json!({"id": kid, "prop": "text", "field": "a b"}),
        json!({"id": kid, "prop": "text", "field": "x", "scope": "bogus"}),
        json!({"id": 999_999_999, "prop": "text", "field": "x"}),
    ] {
        assert!(run(&mut s, json!([{"name": "bind.field", "args": bad}]), json!({})).is_err());
    }
    assert!(crate::host_commands::allowed().contains(&"bind.field"));
}

#[test]
fn a_dry_run_reads_the_pages_back_and_takes_everything_back() {
    let mut s = Session::new();
    let f = page(&mut s);
    let depth = s.undo_depth().0;
    let count = s.doc().unwrap().node_count();
    let r = run(&mut s, json!([{"name": "node.set", "args": {"id": f, "props": {"gap": 8}}}]), json!({"dryRun": true})).expect("dry");
    assert_eq!(r["dryRun"], true);
    assert_eq!(r["pages"][0]["layout"]["gap"], 8.0, "the read-back shows the trial");
    assert_eq!(s.undo_depth().0, depth);
    assert_eq!(s.doc().unwrap().node_count(), count);
    let again = run(&mut s, json!([{"name": "inspect"}]), json!({})).unwrap();
    assert_eq!(again["steps"][0]["result"]["tree"]["children"][0]["layout"]["gap"], 16.0, "the trial left no trace");
    let refused = run(&mut s, json!([{"name": "node.set", "args": {"id": f, "props": {"gap": 20}}}]), json!({"dryRun": true}))
        .expect_err("refused in a dry run too");
    assert!(refused.starts_with("token-refused"), "{refused}");
}

#[test]
fn lifting_the_constraint_frees_the_canvas() {
    let mut s = Session::new();
    let f = page(&mut s);
    s.set_compose_tokens(Some(&Value::Null)).unwrap();
    run(&mut s, json!([{"name": "node.set", "args": {"id": f, "props": {"gap": 20}}}]), json!({})).expect("unconstrained");
}
