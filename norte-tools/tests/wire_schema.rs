//! What the four host tools put on the wire.
//!
//! The argument schema is read by a model, not by a Rust reader. The `///`
//! comments in `norte-tools` are for whoever maintains the executor; the
//! text a model receives is authored with `#[schemars(description = ..)]` and
//! `#[schemars(example = ..)]` (FR-TOOL-6). These tests hold the two audiences
//! apart: an internal requirement code, or an unresolved rustdoc link (it
//! survives as a code span), never reaches the wire.

use norte_core::{ToolRegistry, ToolSpec};
use norte_tools::{EditTool, ReadTool, ShellTool, WriteTool};

fn specs() -> Vec<ToolSpec> {
    let mut registry = ToolRegistry::new();
    registry.register_tool(&ReadTool::new()).expect("read");
    registry.register_tool(&WriteTool::new()).expect("write");
    registry.register_tool(&EditTool::new()).expect("edit");
    registry.register_tool(&ShellTool::new()).expect("command");
    registry.specs()
}

fn spec<'a>(specs: &'a [ToolSpec], name: &str) -> &'a ToolSpec {
    specs
        .iter()
        .find(|spec| spec.name.to_string() == name)
        .expect("the tool is registered")
}

/// Every `description` in the document, at any depth.
fn descriptions(value: &serde_json::Value, found: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "description" {
                    if let Some(text) = child.as_str() {
                        found.push(text.to_owned());
                    }
                }
                descriptions(child, found);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                descriptions(item, found);
            }
        }
        _ => {}
    }
}

#[test]
fn the_model_never_reads_developer_prose() {
    let specs = specs();
    for spec in &specs {
        let mut found = Vec::new();
        descriptions(&spec.parameters, &mut found);
        assert!(
            !found.is_empty(),
            "{}: the arguments reach the model without a word",
            spec.name
        );
        for text in found.into_iter().chain([spec.description.clone()]) {
            assert!(
                !text.contains("FR-"),
                "{}: an internal requirement code reached the model: {text}",
                spec.name
            );
            assert!(
                !text.contains('`'),
                "{}: an unresolved rustdoc link reached the model: {text}",
                spec.name
            );
        }
    }
}

#[test]
fn the_shell_tool_is_flat_so_the_model_cannot_forget_a_tag() {
    // The tool used to take a tagged union — `{"command": {"kind": "shell", ...}}`
    // — and a model that omitted the tag or nested it wrong got a parse error.
    // The schema is one flat object now: `script` at the top, `shell` beside it.
    let specs = specs();
    let parameters = &spec(&specs, "shell").parameters;
    assert!(
        parameters.pointer("/properties/script").is_some(),
        "script is a top-level property: {parameters}"
    );
    assert!(
        parameters.pointer("/properties/shell").is_some(),
        "shell is a top-level property: {parameters}"
    );
    assert!(
        parameters.pointer("/properties/command").is_none(),
        "there is no nested command object: {parameters}"
    );
    assert!(
        parameters.get("oneOf").is_none() && parameters.get("allOf").is_none(),
        "no variant list to guess from: {parameters}"
    );
    // Two shapes that are objects, where the field name alone does not say so.
    assert!(
        spec(&specs, "read")
            .parameters
            .pointer("/properties/lines/examples")
            .expect("a line range carries an example")
            .is_array()
    );
    assert!(
        spec(&specs, "edit")
            .parameters
            .pointer("/properties/edits/examples")
            .expect("an edit carries an example")
            .is_array()
    );
}
