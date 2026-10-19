//! What the four host tools put on the wire.
//!
//! The argument schema is read by a model, not by a Rust reader. The `///`
//! comments in `rustychain-tools` are for whoever maintains the executor; the
//! text a model receives is authored with `#[schemars(description = ..)]` and
//! `#[schemars(example = ..)]` (FR-TOOL-6). These tests hold the two audiences
//! apart: an internal requirement code, or an unresolved rustdoc link (it
//! survives as a code span), never reaches the wire.

use rustychain_core::{ToolRegistry, ToolSpec};
use rustychain_tools::{CommandTool, EditTool, ReadTool, WriteTool};

fn specs() -> Vec<ToolSpec> {
    let mut registry = ToolRegistry::new();
    registry.register_tool(&ReadTool::new()).expect("read");
    registry.register_tool(&WriteTool::new()).expect("write");
    registry.register_tool(&EditTool::new()).expect("edit");
    registry
        .register_tool(&CommandTool::new())
        .expect("command");
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
fn the_model_is_shown_the_nested_shapes() {
    let specs = specs();
    // The tag that separates a program from a script comes from serde, not from
    // schemars: only an example shows the model the key that carries it.
    let program = spec(&specs, "command")
        .parameters
        .pointer("/properties/command/oneOf/0/examples")
        .expect("the program variant carries an example")
        .to_string();
    assert!(program.contains("\"kind\":\"program\""), "{program}");
    // And the tag is a property of the variant, not something the model has to
    // infer from the example: a `oneOf` with no discriminator is an invitation to
    // guess, and a guess costs a request.
    let tag = spec(&specs, "command")
        .parameters
        .pointer("/properties/command/oneOf/0/properties/kind/const")
        .expect("the program variant names its tag")
        .as_str()
        .expect("the tag is a string");
    assert_eq!(tag, "program");
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
