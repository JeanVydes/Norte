use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Display;

#[cfg(feature = "google")]
use crate::{
    FinishReason, Image, Role,
    llm::{FunctionResult, function::FunctionCall},
};

/// A `Inference` in the conversation, which may include text, audio, images, and function calls/results.
/// This is a unified representation that can be converted to/from various LLM formats.
#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Inference {
    pub model: Option<String>,
    pub content: InferenceContent,

    pub thoughts: Vec<Thought>,
    pub function_calls: Vec<FunctionCall>,
    pub function_results: Vec<FunctionResult>,

    pub finish_reason: Option<FinishReason>,
    pub usage: Option<UsageMetadata>,

    pub context: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Thought {
    pub text: String,
    pub context: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageMetadata {
    pub prompt_tokens: Option<i32>,
    pub thought_tokens: Option<i32>,
    pub cached_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceContent {
    pub role: Role,
    pub text: Option<String>,
    pub audio: Option<Vec<u8>>,
    pub images: Option<Vec<Image>>,
}

impl Inference {
    pub fn new(content: impl Into<InferenceContent>) -> Self {
        Self {
            content: content.into(),
            ..Default::default()
        }
    }

    pub fn with_content(role: Role, text: impl ToString) -> Self {
        Self::new(InferenceContent {
            role,
            text: Some(text.to_string()),
            audio: None,
            images: None,
        })
    }

    pub fn as_user(text: impl ToString) -> Self {
        Self::with_content(Role::User, text)
    }

    pub fn as_assistant(text: impl ToString) -> Self {
        Self::with_content(Role::Assistant, text)
    }

    pub fn as_system(text: impl ToString) -> Self {
        Self::with_content(Role::System, text)
    }

    pub fn as_tool(text: impl ToString) -> Self {
        Self::with_content(Role::Tool, text)
    }

    pub fn with_function_results(results: Vec<FunctionResult>) -> Self {
        Self {
            content: InferenceContent {
                role: Role::Tool,
                text: None,
                audio: None,
                images: None,
            },
            function_results: results,
            ..Default::default()
        }
    }

    pub fn add_function_result(mut self, result: FunctionResult) -> Self {
        self.function_results.push(result);
        self
    }

    pub fn add_function_call(mut self, call: FunctionCall) -> Self {
        self.function_calls.push(call);
        self
    }

    pub fn with_audio(mut self, audio: Vec<u8>) -> Self {
        self.content.audio = Some(audio);
        self
    }

    pub fn with_images(mut self, images: Vec<Image>) -> Self {
        self.content.images = Some(images);
        self
    }

    pub fn add_image(mut self, image: Image) -> Self {
        if let Some(imgs) = &mut self.content.images {
            imgs.push(image);
        } else {
            self.content.images = Some(vec![image]);
        }
        self
    }

    pub fn with_thinking(mut self, thinking: String, context: Option<Value>) -> Self {
        self.thoughts.push(Thought {
            text: thinking,
            context,
        });
        self
    }

    pub fn has_thoughts(&self) -> bool {
        !self.thoughts.is_empty()
    }

    pub fn has_function_calls(&self) -> bool {
        !self.function_calls.is_empty()
    }

    pub fn has_function_results(&self) -> bool {
        !self.function_results.is_empty()
    }
}

impl Display for InferenceContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#?}", self)
    }
}

impl Display for Inference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#?}", self)
    }
}

impl From<&str> for Inference {
    fn from(s: &str) -> Self {
        Inference::with_content(Role::User, s)
    }
}

impl From<(Role, &str)> for Inference {
    fn from((role, s): (Role, &str)) -> Self {
        Inference::with_content(role, s)
    }
}

impl From<InferenceContent> for Inference {
    fn from(content: InferenceContent) -> Self {
        Inference::new(content)
    }
}

impl From<&str> for InferenceContent {
    fn from(s: &str) -> Self {
        InferenceContent {
            role: Role::User,
            text: Some(s.to_string()),
            audio: None,
            images: None,
        }
    }
}

impl From<(Role, &str)> for InferenceContent {
    fn from((role, s): (Role, &str)) -> Self {
        InferenceContent {
            role,
            text: Some(s.to_string()),
            audio: None,
            images: None,
        }
    }
}
