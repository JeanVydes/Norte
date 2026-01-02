use schemars::{JsonSchema, Schema, SchemaGenerator, generate::SchemaSettings};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ExtractionError, FunctionCall};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NonNativeFunctionCallingSchema<T> {
    #[schemars(description = "List of function calls made by the model")]
    pub function_calls: Vec<FunctionCall>,
    #[schemars(description = "The original response from the model to the user")]
    pub content: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StringSchema {
    #[schemars(description = "List of function calls made by the model")]
    pub function_calls: Vec<FunctionCall>,
    #[schemars(description = "The original response from the model to the user")]
    pub content: String,
}

/// Converts any schema to be Ollama/llama.cpp compatible by removing unsupported properties.
/// This removes $schema, title, and ensures draft-07 compatibility.
pub fn custom_draft7_for_ollama<T: JsonSchema>() -> Schema {
    let mut settings = SchemaSettings::draft07();
    settings.inline_subschemas = true;
    let r#gen = settings.into_generator();
    let schema = r#gen.into_root_schema_for::<T>();

    clean_schema_for_ollama(schema)
}

/// Cleans an existing schema to be Ollama-compatible.
/// Removes $schema, title, and other metadata that Ollama doesn't accept.
pub fn clean_schema_for_ollama(mut schema: Schema) -> Schema {
    if let Some(obj) = schema.as_object_mut() {
        // Remove Ollama-incompatible properties
        obj.remove("$schema");
        obj.remove("title");
        // Note: We keep $defs if present, as they may be needed for references
    }
    schema
}

impl NonNativeFunctionCallingSchema<()> {
    pub fn new(function_calls: Vec<FunctionCall>) -> Self {
        Self {
            function_calls,
            content: (),
        }
    }

    pub fn new_default() -> Schema {
        let r#gen = SchemaGenerator::default();
        r#gen.into_root_schema_for::<StringSchema>()
    }

    // Ollama-compatible schema for draft-07 with string response
    pub fn new_default_draft7() -> Schema {
        let mut settings = SchemaSettings::draft07();
        settings.inline_subschemas = true;
        let r#gen = settings.into_generator();
        let mut schema = r#gen.into_root_schema_for::<StringSchema>();

        // Clean the schema for Ollama compatibility
        schema = clean_schema_for_ollama(schema);

        // Rebuild as function calling wrapper
        if let Some(obj) = schema.as_object_mut() {
            obj.remove("properties");
            obj.remove("required");

            obj.insert("type".to_string(), Value::String("object".to_string()));

            let mut properties = serde_json::Map::new();

            properties.insert(
                "function_calls".to_string(),
                serde_json::json!({
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "arguments": { "type": "object" },
                            "context": {
                                "anyOf": [
                                    { "type": "string" },
                                    { "type": "null" }
                                ]
                            }
                        },
                        "required": ["name", "arguments"]
                    }
                }),
            );

            properties.insert(
                "content".to_string(),
                serde_json::json!({ "type": "string" }),
            );

            obj.insert("properties".to_string(), Value::Object(properties));
            obj.insert("required".to_string(), serde_json::json!(["content"]));
        }

        schema
    }

    pub fn new_dynamic_inner(runtime_inner_schema: Schema) -> Schema {
        let r#gen = SchemaGenerator::default();
        let mut root_schema = r#gen.into_root_schema_for::<NonNativeFunctionCallingSchema<()>>();

        // Manually override the 'content' property with your runtime schema
        if let Some(obj) = root_schema.as_object_mut()
            && let Some(props) = obj.get_mut("properties")
            && let Some(props_obj) = props.as_object_mut()
        {
            props_obj.insert("content".to_string(), runtime_inner_schema.to_value());
        }

        root_schema
    }

    // Ollama-compatible schema for draft-07 with dynamic inner schema
    // Use this when you have a runtime Schema instead of a compile-time type
    pub fn new_dynamic_inner_draft7(runtime_inner_schema: Schema) -> Schema {
        let mut settings = SchemaSettings::draft07();
        settings.inline_subschemas = true;
        let r#gen = settings.into_generator();
        let mut schema = r#gen.into_root_schema_for::<NonNativeFunctionCallingSchema<()>>();

        // into_root_schema_for returns Schema directly
        if let Some(obj) = schema.as_object_mut() {
            // Remove $schema - Ollama doesn't accept it
            obj.remove("$schema");
            obj.remove("title");

            // Ensure type is set
            if !obj.contains_key("type") {
                obj.insert("type".to_string(), Value::String("object".to_string()));
            }

            // Access and modify properties directly
            if let Some(Value::Object(properties)) = obj.get_mut("properties") {
                // Update function_calls property for Ollama compatibility
                properties.insert(
                    "function_calls".to_string(),
                    serde_json::json!({
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "arguments": { "type": "object" },
                                "context": {
                                    "anyOf": [
                                        { "type": "string" },
                                        { "type": "null" }
                                    ]
                                }
                            },
                            "required": ["name", "arguments"]
                        }
                    }),
                );

                // Replace content property with runtime schema
                properties.insert("content".to_string(), runtime_inner_schema.to_value());
            }

            // Only content is required, function_calls is optional
            obj.insert("required".to_string(), serde_json::json!(["content"]));
        }

        schema
    }
}

/// Extract content field from a JSON response
/// Returns Result to provide detailed error information
pub fn extract_content_from_value<T: for<'de> Deserialize<'de>>(
    value: &Value,
) -> Result<T, ExtractionError> {
    // Ensure we have an object
    let map = value.as_object().ok_or(ExtractionError::NotAnObject)?;

    // Get the content field
    let content_value = map
        .get("content")
        .ok_or_else(|| ExtractionError::FieldNotFound("content".to_string()))?;

    // Deserialize with error propagation
    serde_json::from_value::<T>(content_value.clone()).map_err(|error| {
        ExtractionError::DeserializationError {
            field: "content".to_string(),
            error,
        }
    })
}

/// Extract function calls with strict validation
/// Returns an error if ANY function call is invalid
pub fn extract_function_calls_strict(value: &Value) -> Result<Vec<FunctionCall>, ExtractionError> {
    let map = value.as_object().ok_or(ExtractionError::NotAnObject)?;

    // Get function_calls field (it's optional in the schema)
    let func_calls_value = match map.get("function_calls") {
        Some(v) => v,
        None => return Ok(vec![]), // No function_calls field = empty array
    };

    // Ensure it's an array
    let func_calls_array = func_calls_value.as_array().ok_or_else(|| {
        ExtractionError::WrongType(
            "function_calls".to_string(),
            "array".to_string(),
            type_name(func_calls_value),
        )
    })?;

    // If empty array, return early
    if func_calls_array.is_empty() {
        return Ok(vec![]);
    }

    // Deserialize each call, propagating errors
    let mut function_calls = Vec::with_capacity(func_calls_array.len());
    for (index, item) in func_calls_array.iter().enumerate() {
        let func_call = serde_json::from_value::<FunctionCall>(item.clone())
            .map_err(|error| ExtractionError::InvalidFunctionCall { index, error })?;
        function_calls.push(func_call);
    }

    Ok(function_calls)
}

/// Extract function calls with lenient validation (skip invalid calls)
/// Returns valid calls + warnings about skipped invalid calls
pub fn extract_function_calls_lenient(
    value: &Value,
) -> Result<(Vec<FunctionCall>, Vec<String>), ExtractionError> {
    let map = value.as_object().ok_or(ExtractionError::NotAnObject)?;

    let func_calls_value = match map.get("function_calls") {
        Some(v) => v,
        None => return Ok((vec![], vec![])), // No field = empty
    };

    let func_calls_array = func_calls_value.as_array().ok_or_else(|| {
        ExtractionError::WrongType(
            "function_calls".to_string(),
            "array".to_string(),
            type_name(func_calls_value),
        )
    })?;

    if func_calls_array.is_empty() {
        return Ok((vec![], vec![]));
    }

    let mut function_calls = Vec::new();
    let mut warnings = Vec::new();

    for (index, item) in func_calls_array.iter().enumerate() {
        match serde_json::from_value::<FunctionCall>(item.clone()) {
            Ok(func_call) => function_calls.push(func_call),
            Err(error) => {
                warnings.push(format!(
                    "Skipped invalid function call at index {}: {}",
                    index, error
                ));
            }
        }
    }

    Ok((function_calls, warnings))
}

/// Helper to get JSON type name for error messages
fn type_name(value: &Value) -> String {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
    .to_string()
}

/// Original API - returns Option for backward compatibility
pub fn extract_content_from_value_opt<T: for<'de> Deserialize<'de>>(value: &Value) -> Option<T> {
    extract_content_from_value(value).ok()
}

/// Original API - returns Option, uses lenient extraction
pub fn extract_function_calls_from_value_opt(value: &Value) -> Option<Vec<FunctionCall>> {
    extract_function_calls_lenient(value)
        .ok()
        .map(|(calls, _warnings)| calls)
}
