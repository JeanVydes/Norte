use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::AnyFunction;

#[derive(Clone, Debug)]
pub enum ToolCallingMode {
    None,
    Auto,
    Any,
    Forced(Arc<dyn AnyFunction>),
}

impl Serialize for ToolCallingMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = match self {
            ToolCallingMode::None => "none",
            ToolCallingMode::Auto => "auto",
            ToolCallingMode::Any => "any",
            ToolCallingMode::Forced(tool) => tool.name(),
        };
        serializer.serialize_str(s)
    }
}

impl<'de> Deserialize<'de> for ToolCallingMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "none" => Ok(ToolCallingMode::None),
            "auto" => Ok(ToolCallingMode::Auto),
            "any" => Ok(ToolCallingMode::Any),
            _ => Err(serde::de::Error::custom(format!(
                "Unknown tool calling mode: {}",
                s
            ))),
        }
    }
}

impl PartialEq<str> for ToolCallingMode {
    fn eq(&self, other: &str) -> bool {
        match self {
            ToolCallingMode::None => other == "none",
            ToolCallingMode::Auto => other == "auto",
            ToolCallingMode::Any => other == "any",
            ToolCallingMode::Forced(tool) => tool.name() == other,
        }
    }
}

impl PartialOrd<str> for ToolCallingMode {
    fn partial_cmp(&self, other: &str) -> Option<std::cmp::Ordering> {
        let self_str = match self {
            ToolCallingMode::None => "none",
            ToolCallingMode::Auto => "auto",
            ToolCallingMode::Any => "any",
            ToolCallingMode::Forced(tool) => tool.name(),
        };
        self_str.partial_cmp(other)
    }
}
