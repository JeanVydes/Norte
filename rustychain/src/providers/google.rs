use crate::inference;
use crate::{
    BASE64_ENGINE, FinishReason, Inference, ThinkingMode,
    non_native_function_calling::NonNativeFunctionCallingSchema, prelude::InferenceContent,
    providers::ProviderAbstractionLayer,
};
use base64::Engine;
use secrecy::ExposeSecret;
use secrecy::SecretString;
use serde_json::Value;
use std::{str::FromStr, sync::Arc};

pub struct GoogleProvider {
    base_url: String,
    authorization: Option<SecretString>,
}

impl GoogleProvider {
    pub fn new(base_url: String, authorization: Option<SecretString>) -> Self {
        Self {
            base_url,
            authorization,
        }
    }
}

impl
    ProviderAbstractionLayer<
        gemini_rust::Role,
        gemini_rust::Message,
        gemini_rust::ContentBuilder,
        gemini_rust::FinishReason,
        gemini_rust::Part,
        gemini_rust::generation::model::GenerationResponse,
        i32,
        gemini_rust::Tool,
        gemini_rust::FunctionCall,
        gemini_rust::FunctionCallingMode,
        gemini_rust::Gemini,
    > for GoogleProvider
{
    fn client(&self) -> crate::Result<gemini_rust::Gemini> {
        let client = gemini_rust::GeminiBuilder::new(
            self.authorization
                .as_ref()
                .map(|s| s.expose_secret())
                .unwrap_or(&"".to_string()),
        )
        .with_base_url(url::Url::from_str(&self.base_url)?);

        client
            .build()
            .map_err(|e| crate::Error::Generic(format!("Failed to create Gemini client: {}", e)))
    }

    fn to_native_role(role: &gemini_rust::Role) -> crate::Role {
        match role {
            gemini_rust::Role::User => crate::Role::User,
            gemini_rust::Role::Model => crate::Role::Assistant,
        }
    }

    fn to_provider_role(role: &crate::Role) -> gemini_rust::Role {
        match role {
            crate::Role::User => gemini_rust::Role::User,
            crate::Role::Assistant => gemini_rust::Role::Model,
            crate::Role::Tool => gemini_rust::Role::User,
            crate::Role::System => gemini_rust::Role::Model,
            crate::Role::Developer => gemini_rust::Role::User,
        }
    }

    fn to_native_finish_reason(finish: &gemini_rust::FinishReason) -> crate::FinishReason {
        match finish {
            gemini_rust::FinishReason::Stop => FinishReason::Stop,
            gemini_rust::FinishReason::MaxTokens => FinishReason::MaxTokens,
            gemini_rust::FinishReason::Safety => FinishReason::Safety,
            gemini_rust::FinishReason::Recitation => FinishReason::Recitation,
            gemini_rust::FinishReason::Blocklist => FinishReason::Blocklist,
            gemini_rust::FinishReason::ProhibitedContent => FinishReason::ProhibitedContent,
            gemini_rust::FinishReason::Spii => FinishReason::Spii,
            gemini_rust::FinishReason::MalformedFunctionCall => FinishReason::MalformedFunctionCall,
            gemini_rust::FinishReason::UnexpectedToolCall => FinishReason::UnexpectedToolCall,
            gemini_rust::FinishReason::TooManyToolCalls => FinishReason::TooManyToolCalls,
            gemini_rust::FinishReason::Language => FinishReason::Other("Language".to_string()),
            gemini_rust::FinishReason::ImageSafety => FinishReason::ImageSafety,
            gemini_rust::FinishReason::Other => FinishReason::Other("Other".to_string()),
            gemini_rust::FinishReason::FinishReasonUnspecified => FinishReason::Unspecified,
        }
    }

    fn to_provider_finish_reason(finish: &FinishReason) -> gemini_rust::FinishReason {
        match finish {
            FinishReason::Stop => gemini_rust::FinishReason::Stop,
            FinishReason::MaxTokens => gemini_rust::FinishReason::MaxTokens,
            FinishReason::Safety => gemini_rust::FinishReason::Safety,
            FinishReason::Recitation => gemini_rust::FinishReason::Recitation,
            FinishReason::Blocklist => gemini_rust::FinishReason::Blocklist,
            FinishReason::ProhibitedContent => gemini_rust::FinishReason::ProhibitedContent,
            FinishReason::Spii => gemini_rust::FinishReason::Spii,
            FinishReason::MalformedFunctionCall => gemini_rust::FinishReason::MalformedFunctionCall,
            FinishReason::UnexpectedToolCall => gemini_rust::FinishReason::UnexpectedToolCall,
            FinishReason::TooManyToolCalls => gemini_rust::FinishReason::TooManyToolCalls,
            FinishReason::ToolCall => gemini_rust::FinishReason::Other,
            FinishReason::ImageSafety => gemini_rust::FinishReason::ImageSafety,
            FinishReason::Other(_) => gemini_rust::FinishReason::Other,
            FinishReason::Unspecified => gemini_rust::FinishReason::FinishReasonUnspecified,
        }
    }

    fn to_native_image(image: &gemini_rust::Part) -> crate::Result<crate::Image> {
        if let gemini_rust::Part::InlineData { inline_data } = image {
            match inline_data.mime_type.as_str() {
                "image/png" | "image/jpeg" | "image/jpg" | "image/gif" => Ok(crate::Image {
                    url: format!("data:{};base64,{}", inline_data.mime_type, inline_data.data),
                    text: None,
                }),
                _ => Err(crate::Error::Generic(
                    "Unsupported image MIME type".to_string(),
                )),
            }
        } else {
            Err(crate::Error::Generic(
                "Provided part is not an InlineData part".to_string(),
            ))
        }
    }

    fn to_provider_image(image: &crate::Image) -> crate::Result<gemini_rust::Part> {
        let base64_data = if image.url.starts_with("data:") {
            // Extract base64 part from data URL
            image
                .url
                .split_once(",")
                .map(|x| x.1)
                .unwrap_or("")
                .to_string()
        } else {
            image.url.clone()
        };

        Ok(gemini_rust::Part::InlineData {
            inline_data: gemini_rust::Blob::new("image/png", base64_data),
        })
    }

    fn to_provider_thinking_mode(mode: &ThinkingMode) -> crate::Result<i32> {
        Ok(match mode {
            ThinkingMode::None => 0,
            ThinkingMode::Sized(size) => *size,
            // Google doesnt support effort levels directly, set them dynamically
            _ => -1,
        })
    }

    fn to_native_function_call(
        call: &gemini_rust::FunctionCall,
    ) -> crate::Result<crate::FunctionCall> {
        Ok(crate::FunctionCall {
            name: call.name.clone(),
            arguments: call.args.clone(),
            context: call.thought_signature.clone(),
        })
    }

    fn to_provider_function_call(
        call: &crate::FunctionCall,
    ) -> crate::Result<gemini_rust::FunctionCall> {
        Ok(gemini_rust::FunctionCall {
            name: call.name.clone(),
            args: call.arguments.clone(),
            thought_signature: call.context.clone(),
        })
    }

    fn to_provider_tool_calling_mode(
        mode: &crate::llm::tool_calling_mode::ToolCallingMode,
    ) -> gemini_rust::FunctionCallingMode {
        match mode {
            crate::ToolCallingMode::None => gemini_rust::FunctionCallingMode::None,
            crate::ToolCallingMode::Auto => gemini_rust::FunctionCallingMode::Auto,
            _ => gemini_rust::FunctionCallingMode::Auto,
        }
    }

    fn to_inference_from_response(
        response: &gemini_rust::generation::model::GenerationResponse,
        config: Option<Arc<crate::GenerationConfig>>,
    ) -> Inference {
        let candidate = match response.candidates.first() {
            Some(c) => c,
            None => return Inference::default(),
        };

        let mut function_calls = vec![];
        let mut message_parts = vec![];
        let mut audio_parts = vec![];
        let mut thoughts: Vec<crate::inference::Thought> = vec![];
        if let Some(parts) = &candidate.content.parts {
            for part in parts {
                if let gemini_rust::Part::FunctionCall { function_call, .. } = part {
                    function_calls.push(Self::to_native_function_call(function_call).unwrap());
                }

                if let gemini_rust::Part::Text {
                    text,
                    thought,
                    thought_signature,
                } = part
                {
                    if let Some(true) = thought {
                        thoughts.push(crate::inference::Thought {
                            text: text.clone(),
                            context: thought_signature.as_ref().map(|s| Value::String(s.clone())),
                        });
                    } else if let Some(cfg) = &config
                        && !cfg.native_tool_handling
                    {
                        use crate::non_native_function_calling::extract_content_from_value_opt;

                        if let Ok(value) = serde_json::from_str::<Value>(text) {
                            if let Some(inner_content) = extract_content_from_value_opt(&value) {
                                message_parts.push(inner_content);
                            } else {
                                message_parts.push(text.clone());
                            }

                            if let Some(calls) =
                                    crate::non_native_function_calling::extract_function_calls_from_value_opt(
                                        &value,
                                    )
                                {
                                    for call in calls {
                                        function_calls.push(call);
                                    }
                                }
                        } else {
                            message_parts.push(text.clone());
                        }
                    } else {
                        message_parts.push(text.clone());
                    }
                }

                if let gemini_rust::Part::InlineData { inline_data } = part
                    && inline_data.mime_type.starts_with("audio/")
                {
                    let decoded = base64::engine::general_purpose::STANDARD
                        .decode(&inline_data.data)
                        .ok();
                    if let Some(audio_data) = decoded {
                        audio_parts.push(audio_data);
                    }
                }

                if let gemini_rust::Part::FunctionResponse { .. } = part {
                    // Currently, we do not extract function results from Gemini responses.
                }
            }
        }

        let finish_reason = candidate
            .finish_reason
            .as_ref()
            .map(Self::to_native_finish_reason);

        let usage: Option<inference::UsageMetadata> = if let Some(usage) = &response.usage_metadata {
            Some(inference::UsageMetadata {
                prompt_tokens: usage.prompt_token_count,
                thought_tokens: usage.thoughts_token_count,
                cached_tokens: usage.cached_content_token_count,
                total_tokens: usage.total_token_count,
            })
        } else {
            None
        };

        Inference {
            model: response.model_version.clone(),
            content: InferenceContent {
                role: Self::to_native_role(
                    &candidate
                        .content
                        .role
                        .clone()
                        .unwrap_or(gemini_rust::Role::Model),
                ),
                text: if message_parts.is_empty() {
                    None
                } else {
                    Some(message_parts.join("\n"))
                },
                audio: audio_parts.into_iter().next(),
                images: None,
            },
            thoughts,
            function_calls,
            finish_reason,
            usage,
            context: serde_json::to_value(&candidate.content).ok(),
            function_results: vec![], // Currently, we do not extract function results from Gemini responses.
        }
    }

    fn to_provider_message(inference: &crate::Inference) -> gemini_rust::Message {
        let role = Self::to_provider_role(&inference.content.role);

        let mut content: gemini_rust::Content = gemini_rust::Content {
            role: Some(role.clone()),
            parts: vec![].into(),
        };

        if let Some(audio) = &inference.content.audio
            && let Some(parts) = &mut content.parts
        {
            let base64 = BASE64_ENGINE.encode(audio);
            parts.push(gemini_rust::Part::InlineData {
                inline_data: gemini_rust::Blob::new("audio/mp3", base64),
            });
        }

        if let Some(msg) = &inference.content.text
            && let Some(parts) = &mut content.parts
        {
            let is_thought = !inference.thoughts.is_empty();
            parts.push(gemini_rust::Part::Text {
                text: msg.clone(),
                thought: if is_thought { Some(true) } else { None },
                thought_signature: inference
                    .thoughts
                    .first()
                    .and_then(|t| t.context.as_ref())
                    .and_then(|ctx| ctx.as_str())
                    .map(|s| s.to_string()),
            });
        }

        if !inference.function_calls.is_empty()
            && let Some(parts) = &mut content.parts
        {
            for fc in &inference.function_calls {
                parts.push(gemini_rust::Part::FunctionCall {
                    function_call: Self::to_provider_function_call(fc).unwrap(),
                    thought_signature: fc.context.clone(),
                });
            }
        }

        if !inference.function_results.is_empty()
            && let Some(parts) = &mut content.parts
        {
            for fr in &inference.function_results {
                parts.push(gemini_rust::Part::FunctionResponse {
                    function_response: gemini_rust::FunctionResponse {
                        name: fr.name.clone(),
                        response: Some(fr.results.clone()),
                    },
                });
            }
        }

        gemini_rust::Message { role, content }
    }

    fn new_chat_request(
        &self,
        _req: super::ChatCompletionRequest,
    ) -> crate::Result<gemini_rust::ContentBuilder> {
        let client = self.client()?;

        let mut history: Vec<gemini_rust::Message> = _req
            .messages
            .iter()
            .map(Self::to_provider_message)
            .collect();
        history.push(Self::to_provider_message(_req.inference));

        let mut req = client
            .generate_content()
            .with_messages(history.clone())
            .with_thinking_budget(Self::to_provider_thinking_mode(&_req.config.thinking_mode)?)
            .with_generation_config(gemini_rust::GenerationConfig {
                temperature: Some(_req.config.temperature),
                top_p: Some(_req.config.top_p),
                top_k: Some(_req.config.top_k),
                max_output_tokens: Some(_req.config.max_output_tokens),
                thinking_config: Some(gemini_rust::ThinkingConfig {
                    thinking_budget: Some(GoogleProvider::to_provider_thinking_mode(
                        &_req.config.thinking_mode,
                    )?),
                    include_thoughts: Some(_req.config.include_thoughts),
                }),
                candidate_count: Some(_req.config.candidate_count),
                ..Default::default()
            });

        // Schema & Tool injection logic
        match (&_req.config.output_schema, _req.config.native_tool_handling) {
            (Some(schema), false) => {
                let wrapped = NonNativeFunctionCallingSchema::new_dynamic_inner(schema.clone());
                req = req
                    .with_response_mime_type("application/json".to_owned())
                    .with_response_schema(serde_json::to_value(wrapped).unwrap_or_default());
            }
            (Some(schema), true) => {
                req = req
                    .with_response_mime_type("application/json".to_owned())
                    .with_response_schema(serde_json::to_value(schema.clone()).unwrap_or_default());
            }
            (None, false) => {
                let default_schema = NonNativeFunctionCallingSchema::new_default();
                req = req
                    .with_response_mime_type("application/json".to_owned())
                    .with_response_schema(serde_json::to_value(default_schema).unwrap_or_default());
            }
            (None, true) => {} // No action needed for native tools without explicit output schema
        }

        if let Some(stop_sequences) = &_req.config.stop_sequences {
            req = req.with_stop_sequences(stop_sequences.clone());
        }

        let mut system_prompt = _req.system_prompt.to_string();
        if _req.config.native_tool_handling {
            req = req.with_function_calling_mode(Self::to_provider_tool_calling_mode(
                &_req.config.tool_calling_mode,
            ));
            for tool in _req.tools {
                req = req.with_tool(tool.to_google());
            }
        } else {
            use gemini_rust::FunctionCallingMode;

            req = req.with_function_calling_mode(FunctionCallingMode::None);
            system_prompt.push_str(&format!(
                "\n\nThe following tools are available to you:\n{}\n\n",
                crate::util::tools_to_string(_req.tools)
            ));
        }

        req = req.with_system_prompt(system_prompt);
        Ok(req)
    }
}
