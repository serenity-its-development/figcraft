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

#[test]
fn section_insert_places_a_locked_block_the_host_judges_and_the_sidecar_is_typed() {
    let mut s = Session::new();
    let f = page(&mut s);
    let node = json!({"c": "form", "p": {"title": "Contact us"}}).to_string();
    let r = run(&mut s, json!([{"name": "section.insert", "args": {"node": node, "parent": f}}]), json!({})).expect("an untrusted run may insert a block");
    let made = r["steps"][0]["result"].clone();
    assert_eq!(made["c"], "form");
    assert_eq!(made["oneosId"], "form-1");
    let doc = s.doc().unwrap();
    let layer = doc.node(crate::NodeId(made["id"].as_u64().unwrap())).unwrap();
    assert_eq!(layer.plugin_data.get("oneos.locked").map(String::as_str), Some("1"));
    assert_eq!(layer.plugin_data.get("oneos.id").map(String::as_str), Some("form-1"));
    assert_eq!(serde_json::from_str::<Value>(layer.plugin_data.get("oneos.node").unwrap()).unwrap(), json!({"c": "form", "p": {"title": "Contact us"}}));
    // The label child is decor the bridge never reads back.
    assert_eq!(doc.node(layer.children[0]).unwrap().plugin_data.get("oneos.decor").map(String::as_str), Some("1"));
    // A second block of the same kind gets the next id; a position can be stated.
    let r2 = run(&mut s, json!([{"name": "section.insert", "args": {"node": json!({"c": "form"}).to_string(), "parent": f, "index": 0}}]), json!({})).unwrap();
    assert_eq!(r2["steps"][0]["result"]["oneosId"], "form-2");
    let kids = s.doc().unwrap().node(crate::NodeId(f)).unwrap().children.clone();
    assert_eq!(kids[0].0, r2["steps"][0]["result"]["id"].as_u64().unwrap());
    // The bare sidecar write is still refused to a free run.
    let e = run(&mut s, json!([{"name": "node.set", "args": {"id": made["id"], "props": {"pluginData": {"oneos.node": "{}"}}}}]), json!({})).expect_err("free write");
    assert!(e.contains("oneos.* sidecar"), "{e}");
    assert!(crate::host_commands::allowed().contains(&"section.insert"));
}

#[test]
fn section_insert_refuses_what_the_canvas_draws_or_cannot_place() {
    let mut s = Session::new();
    let f = page(&mut s);
    for (node, parent, why) in [
        (json!({"c": "heading", "p": {"text": "x", "level": 1}}).to_string(), f, "drawn and edited on the canvas"),
        (json!({"c": "form", "kids": ["a"]}).to_string(), f, "not 'kids'"),
        (json!({"c": "Form"}).to_string(), f, "compose block name"),
        ("not json".to_string(), f, "JSON text"),
        (json!({"c": "form"}).to_string(), 999_999_999, "no layer"),
    ] {
        let e = run(&mut s, json!([{"name": "section.insert", "args": {"node": node, "parent": parent}}]), json!({})).expect_err(why);
        assert!(e.contains(why), "{why}: {e}");
    }
    // A layer that is not on a compose page, and a frame that is not the Expanded one.
    let loose = run(&mut s, json!([{"name": "node.create", "args": {"type": "frame", "name": "Loose", "select": false, "x": 900, "y": 0, "width": 50, "height": 50}}]), json!({"trusted": true})).unwrap();
    let loose = loose["steps"][0]["result"]["id"].as_u64().unwrap();
    let e = run(&mut s, json!([{"name": "section.insert", "args": {"node": json!({"c": "form"}).to_string(), "parent": loose}}]), json!({})).expect_err("off page");
    assert!(e.contains("not on a compose page"), "{e}");
    run(&mut s, json!([{"name": "node.set", "args": {"id": f, "props": {"pluginData": {"oneos.cls": "compact"}}}}]), json!({"trusted": true})).unwrap();
    let e = run(&mut s, json!([{"name": "section.insert", "args": {"node": json!({"c": "form"}).to_string(), "parent": f}}]), json!({})).expect_err("compact");
    assert!(e.contains("Expanded frame"), "{e}");
}

#[test]
fn the_editor_is_offered_only_tokens() {
    let mut s = Session::new();
    let f = page(&mut s);
    let kid = s.doc().unwrap().node(crate::NodeId(f)).unwrap().children[0];
    let ids = [crate::NodeId(f)];
    let spacing = s.compose_scale(&ids, "gap").expect("a page layer's gap is a scale");
    assert_eq!(spacing.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["none", "sm", "md", "lg", "xl"]);
    assert_eq!(s.compose_scale(&ids, "paddingLeft"), Some(spacing.clone()));
    assert_eq!(s.compose_scale(&ids, "cornerRadius").unwrap().iter().map(|(_, v)| *v).collect::<Vec<_>>(), [0.0, 8.0, 12.0]);
    assert_eq!(s.compose_scale(&[kid], "fontSize").unwrap().iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["body", "h3", "h1"]);
    assert_eq!(s.compose_scale(&ids, "width"), None, "a size is not a token");
    assert_eq!(s.compose_colors(&ids).unwrap().iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["accent", "ink", "surface", "white"]);
    // A drag lands on the nearest token and a token is then accepted by the engine.
    assert_eq!(s.compose_snap(&ids, "gap", 19.0), 16.0);
    assert_eq!(s.compose_snap(&ids, "gap", 21.0), 24.0);
    assert_eq!(s.compose_snap(&ids, "gap", 1.0), 0.0);
    assert_eq!(s.compose_snap(&ids, "width", 19.0), 19.0);
    let snapped = s.compose_snap(&ids, "gap", 19.7);
    run(&mut s, json!([{"name": "node.set", "args": {"id": f, "props": {"gap": snapped}}}]), json!({})).expect("a snapped value is a token");
    // Off a page, or with no tokens, nothing is offered.
    let loose = run(&mut s, json!([{"name": "node.create", "args": {"type": "frame", "name": "Loose", "select": false, "x": 900, "y": 0, "width": 50, "height": 50}}]), json!({"trusted": true})).unwrap();
    let loose = crate::NodeId(loose["steps"][0]["result"]["id"].as_u64().unwrap());
    assert_eq!(s.compose_scale(&[loose], "gap"), None);
    s.set_compose_tokens(Some(&Value::Null)).unwrap();
    assert_eq!(s.compose_scale(&ids, "gap"), None);
    assert_eq!(s.compose_colors(&ids), None);
}
