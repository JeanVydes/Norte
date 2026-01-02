use async_openai::types::{
    chat::{
        ChatCompletionAllowedTools, ChatCompletionAllowedToolsChoice,
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestAssistantMessageContentPart,
        ChatCompletionRequestDeveloperMessageContent,
        ChatCompletionRequestDeveloperMessageContentPart,
        ChatCompletionRequestMessageContentPartText, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestSystemMessageContentPart,
        ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
        ChatCompletionRequestToolMessageContentPart, ChatCompletionRequestUserMessageContent,
        ChatCompletionRequestUserMessageContentPart, ChatCompletionTool, FunctionObject,
        ResponseFormat, ResponseFormatJsonSchema, StopConfiguration, ToolChoiceAllowedMode,
    },
    embeddings::{CreateEmbeddingRequest, EncodingFormat},
    responses::{ReasoningTextContent, TextContent},
};
use base64::Engine;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::sync::Arc;

use crate::{
    inference, non_native_function_calling::NonNativeFunctionCallingSchema,
    providers::ProviderAbstractionLayer,
};

pub struct OpenAICompatibleProvider {
    base_url: String,
    authorization: Option<SecretString>,
}

impl OpenAICompatibleProvider {
    pub fn new(base_url: String, authorization: Option<SecretString>) -> Self {
        Self {
            base_url,
            authorization,
        }
    }

    pub fn chat_role_to_message_role(
        role: &async_openai::types::chat::Role,
    ) -> async_openai::types::responses::MessageRole {
        match role {
            async_openai::types::chat::Role::User => {
                async_openai::types::responses::MessageRole::User
            }
            async_openai::types::chat::Role::Assistant => {
                async_openai::types::responses::MessageRole::Assistant
            }
            async_openai::types::chat::Role::Tool => {
                async_openai::types::responses::MessageRole::Tool
            }
            async_openai::types::chat::Role::System => {
                async_openai::types::responses::MessageRole::System
            }
            async_openai::types::chat::Role::Function => {
                async_openai::types::responses::MessageRole::Tool
            }
        }
    }

    pub fn to_chat_completion_tools(
        tools: &[Arc<dyn crate::AnyFunction>],
    ) -> Vec<async_openai::types::chat::ChatCompletionTools> {
        let mut provider_tools = vec![];

        for tool in tools {
            let func =
                async_openai::types::chat::ChatCompletionTools::Function(ChatCompletionTool {
                    function: FunctionObject {
                        name: tool.name().to_string(),
                        description: Some(tool.description().to_string()),
                        parameters: Some(tool.parameters_schema().clone().to_value()),
                        strict: None,
                    },
                });

            provider_tools.push(func);
        }

        provider_tools
    }

    pub fn to_inference_from_stream_response(
        response: &async_openai::types::chat::CreateChatCompletionStreamResponse,
        config: Option<Arc<crate::GenerationConfig>>,
    ) -> crate::Inference {
        let choice = match response.choices.first() {
            Some(c) => c,
            None => return crate::Inference::default(),
        };

        let mut function_calls = vec![];
        let mut message_parts = vec![];
        let thoughts: Vec<crate::inference::Thought> = vec![];
        let finish_reason = choice
            .finish_reason
            .map(|f| Self::to_native_finish_reason(&f));

        if let Some(text) = &choice.delta.content {
            if let Some(cfg) = &config
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

        if let Some(tool_calls) = &choice.delta.tool_calls {
            for tc in tool_calls {
                if let Some(func) = &tc.function {
                    function_calls.push(crate::FunctionCall {
                        name: func.name.clone().unwrap_or_default(),
                        arguments: serde_json::to_value(&func.arguments).unwrap_or_default(),
                        context: tc.id.clone(),
                    });
                }
            }
        }

        crate::Inference {
            model: Some(response.model.clone()),
            content: crate::prelude::InferenceContent {
                role: Self::to_native_role(&Self::chat_role_to_message_role(
                    &choice
                        .delta
                        .role
                        .unwrap_or(async_openai::types::chat::Role::Assistant),
                )),
                text: if message_parts.is_empty() {
                    None
                } else {
                    Some(message_parts.join("\n"))
                },
                audio: None,
                images: None,
            },
            thoughts,
            function_calls,
            finish_reason,
            ..Default::default()
        }
    }

    pub fn new_embedding_request(
        model: impl ToString,
        dimensions: u32,
        input: Vec<impl ToString>,
    ) -> crate::Result<CreateEmbeddingRequest> {
        Ok(CreateEmbeddingRequest {
            model: model.to_string(),
            input: async_openai::types::embeddings::EmbeddingInput::StringArray(
                input.into_iter().map(|s| s.to_string()).collect(),
            ),
            dimensions: Some(dimensions),
            encoding_format: Some(EncodingFormat::Float),
            user: None,
        })
    }

    fn extract_text_content_parts(
        content_parts: &[async_openai::types::responses::MessageContent],
    ) -> Vec<ChatCompletionRequestMessageContentPartText> {
        content_parts
            .iter()
            .filter_map(|c| {
                if let async_openai::types::responses::MessageContent::Text(tc) = c {
                    Some(ChatCompletionRequestMessageContentPartText {
                        text: tc.text.clone(),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    fn build_tool_message(
        inference: &crate::Inference,
    ) -> async_openai::types::chat::ChatCompletionRequestMessage {
        let tool_call_id = inference
            .function_results
            .first()
            .and_then(|fr| fr.context.clone())
            .unwrap_or_default();

        let content = inference
            .function_results
            .iter()
            .map(|fr| {
                ChatCompletionRequestToolMessageContentPart::Text(
                    ChatCompletionRequestMessageContentPartText {
                        text: serde_json::to_string(&fr.results).unwrap_or_default(),
                    },
                )
            })
            .collect();

        async_openai::types::chat::ChatCompletionRequestMessage::Tool(
            ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Array(content),
                tool_call_id,
            },
        )
    }

    fn build_user_message(
        content_parts: &[async_openai::types::responses::MessageContent],
    ) -> async_openai::types::chat::ChatCompletionRequestMessage {
        let content = Self::extract_text_content_parts(content_parts)
            .into_iter()
            .map(ChatCompletionRequestUserMessageContentPart::Text)
            .collect();

        async_openai::types::chat::ChatCompletionRequestMessage::User(
            async_openai::types::chat::ChatCompletionRequestUserMessage {
                name: None,
                content: ChatCompletionRequestUserMessageContent::Array(content),
            },
        )
    }

    fn build_system_message(
        content_parts: &[async_openai::types::responses::MessageContent],
    ) -> async_openai::types::chat::ChatCompletionRequestMessage {
        let content = Self::extract_text_content_parts(content_parts)
            .into_iter()
            .map(ChatCompletionRequestSystemMessageContentPart::Text)
            .collect();

        async_openai::types::chat::ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessage {
                name: None,
                content: ChatCompletionRequestSystemMessageContent::Array(content),
            },
        )
    }

    fn build_developer_message(
        content_parts: &[async_openai::types::responses::MessageContent],
    ) -> async_openai::types::chat::ChatCompletionRequestMessage {
        let content = Self::extract_text_content_parts(content_parts)
            .into_iter()
            .map(ChatCompletionRequestDeveloperMessageContentPart::Text)
            .collect();

        async_openai::types::chat::ChatCompletionRequestMessage::Developer(
            async_openai::types::chat::ChatCompletionRequestDeveloperMessage {
                name: None,
                content: ChatCompletionRequestDeveloperMessageContent::Array(content),
            },
        )
    }

    fn build_assistant_message(
        content_parts: &[async_openai::types::responses::MessageContent],
        tool_calls: Vec<ChatCompletionMessageToolCalls>,
    ) -> async_openai::types::chat::ChatCompletionRequestMessage {
        let content = Self::extract_text_content_parts(content_parts)
            .into_iter()
            .map(ChatCompletionRequestAssistantMessageContentPart::Text)
            .collect::<Vec<_>>();

        async_openai::types::chat::ChatCompletionRequestMessage::Assistant(
            ChatCompletionRequestAssistantMessage {
                name: None,
                audio: None,
                refusal: None,
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                content: if content.is_empty() {
                    Some(ChatCompletionRequestAssistantMessageContent::Array(vec![]))
                } else {
                    Some(ChatCompletionRequestAssistantMessageContent::Array(content))
                },
                #[allow(deprecated)]
                function_call: None,
            },
        )
    }
}

impl
    ProviderAbstractionLayer<
        async_openai::types::responses::MessageRole,
        async_openai::types::chat::ChatCompletionRequestMessage,
        async_openai::types::chat::CreateChatCompletionRequestArgs,
        async_openai::types::chat::FinishReason,
        async_openai::types::chat::ImageUrl,
        async_openai::types::chat::CreateChatCompletionResponse,
        async_openai::types::chat::ReasoningEffort,
        async_openai::types::chat::ChatCompletionTools,
        async_openai::types::chat::FunctionCall,
        async_openai::types::chat::ChatCompletionToolChoiceOption,
        async_openai::Client<async_openai::config::OpenAIConfig>,
    > for OpenAICompatibleProvider
{
    fn client(&self) -> crate::Result<async_openai::Client<async_openai::config::OpenAIConfig>> {
        let mut config =
            async_openai::config::OpenAIConfig::new().with_api_base(self.base_url.clone());
        if let Some(auth) = &self.authorization {
            config = config.with_api_key(auth.expose_secret());
        }

        Ok(async_openai::Client::with_config(config))
    }

    fn to_native_role(role: &async_openai::types::responses::MessageRole) -> crate::Role {
        match role {
            async_openai::types::responses::MessageRole::User => crate::Role::User,
            async_openai::types::responses::MessageRole::Assistant => crate::Role::Assistant,
            async_openai::types::responses::MessageRole::System => crate::Role::System,
            async_openai::types::responses::MessageRole::Tool => crate::Role::Tool,
            async_openai::types::responses::MessageRole::Developer => crate::Role::Developer,
            async_openai::types::responses::MessageRole::Discriminator => crate::Role::Assistant,
            async_openai::types::responses::MessageRole::Critic => crate::Role::Assistant,
            async_openai::types::responses::MessageRole::Unknown => crate::Role::Assistant,
        }
    }

    fn to_provider_role(role: &crate::Role) -> async_openai::types::responses::MessageRole {
        match role {
            crate::Role::User => async_openai::types::responses::MessageRole::User,
            crate::Role::Assistant => async_openai::types::responses::MessageRole::Assistant,
            crate::Role::Tool => async_openai::types::responses::MessageRole::Tool,
            crate::Role::System => async_openai::types::responses::MessageRole::System,
            crate::Role::Developer => async_openai::types::responses::MessageRole::Developer,
        }
    }

    fn to_native_finish_reason(
        finish: &async_openai::types::chat::FinishReason,
    ) -> crate::FinishReason {
        match finish {
            async_openai::types::chat::FinishReason::Stop => crate::FinishReason::Stop,
            async_openai::types::chat::FinishReason::Length => crate::FinishReason::MaxTokens,
            async_openai::types::chat::FinishReason::FunctionCall => crate::FinishReason::ToolCall,
            async_openai::types::chat::FinishReason::ContentFilter => crate::FinishReason::Safety,
            async_openai::types::chat::FinishReason::ToolCalls => crate::FinishReason::ToolCall,
        }
    }

    fn to_provider_finish_reason(
        finish: &crate::FinishReason,
    ) -> async_openai::types::chat::FinishReason {
        match finish {
            crate::FinishReason::Stop => async_openai::types::chat::FinishReason::Stop,
            crate::FinishReason::MaxTokens => async_openai::types::chat::FinishReason::Length,
            crate::FinishReason::ToolCall => async_openai::types::chat::FinishReason::ToolCalls,
            crate::FinishReason::Safety => async_openai::types::chat::FinishReason::ContentFilter,
            _ => async_openai::types::chat::FinishReason::Stop,
        }
    }

    fn to_native_image(image: &async_openai::types::chat::ImageUrl) -> crate::Result<crate::Image> {
        Ok(crate::Image {
            url: image.url.clone(),
            text: image
                .detail
                .clone()
                .map(|d| serde_json::to_string(&d).unwrap_or_default()),
        })
    }

    fn to_provider_image(
        image: &crate::Image,
    ) -> crate::Result<async_openai::types::chat::ImageUrl> {
        Ok(async_openai::types::chat::ImageUrl {
            url: image.url.clone(),
            detail: image
                .text
                .as_ref()
                .and_then(|text| serde_json::from_str(text).ok()),
        })
    }

    fn to_provider_thinking_mode(
        mode: &crate::ThinkingMode,
    ) -> crate::Result<async_openai::types::chat::ReasoningEffort> {
        Ok(match mode {
            crate::ThinkingMode::None => async_openai::types::chat::ReasoningEffort::None,
            crate::ThinkingMode::Effort(effort) => match effort {
                crate::thinking_mode::ThinkingEffort::Low => {
                    async_openai::types::chat::ReasoningEffort::Low
                }
                crate::thinking_mode::ThinkingEffort::Medium => {
                    async_openai::types::chat::ReasoningEffort::Medium
                }
                crate::thinking_mode::ThinkingEffort::High => {
                    async_openai::types::chat::ReasoningEffort::High
                }
            },
            _ => async_openai::types::chat::ReasoningEffort::Minimal,
        })
    }

    fn to_native_function_call(
        call: &async_openai::types::chat::FunctionCall,
    ) -> crate::Result<crate::FunctionCall> {
        Ok(crate::FunctionCall {
            name: call.name.clone(),
            arguments: serde_json::from_str(&call.arguments).unwrap_or_default(),
            context: None,
        })
    }

    fn to_provider_tool_calling_mode(
        mode: &crate::llm::tool_calling_mode::ToolCallingMode,
    ) -> async_openai::types::chat::ChatCompletionToolChoiceOption {
        match mode {
            crate::ToolCallingMode::None => {
                async_openai::types::chat::ChatCompletionToolChoiceOption::Mode(
                    async_openai::types::chat::ToolChoiceOptions::None,
                )
            }
            crate::ToolCallingMode::Auto => {
                async_openai::types::chat::ChatCompletionToolChoiceOption::Mode(
                    async_openai::types::chat::ToolChoiceOptions::Auto,
                )
            }
            crate::ToolCallingMode::Any => {
                async_openai::types::chat::ChatCompletionToolChoiceOption::Mode(
                    async_openai::types::chat::ToolChoiceOptions::Required,
                )
            }
            crate::ToolCallingMode::Forced(tool) => {
                async_openai::types::chat::ChatCompletionToolChoiceOption::AllowedTools(
                    ChatCompletionAllowedToolsChoice {
                        allowed_tools: vec![ChatCompletionAllowedTools {
                            mode: ToolChoiceAllowedMode::Auto,
                            tools: vec![tool.to_generic_tool()],
                        }],
                    },
                )
            }
        }
    }

    fn to_provider_function_call(
        call: &crate::FunctionCall,
    ) -> crate::Result<async_openai::types::chat::FunctionCall> {
        Ok(async_openai::types::chat::FunctionCall {
            name: call.name.clone(),
            arguments: serde_json::from_value(call.arguments.clone()).unwrap_or_default(),
        })
    }

    fn to_inference_from_response(
        response: &async_openai::types::chat::CreateChatCompletionResponse,
        config: Option<Arc<crate::GenerationConfig>>,
    ) -> crate::Inference {
        let choice = match response.choices.first() {
            Some(c) => c,
            None => return crate::Inference::default(),
        };

        let mut function_calls = vec![];
        let mut message_parts = vec![];
        let mut audio_parts = vec![];
        let mut thoughts: Vec<crate::inference::Thought> = vec![];
        let finish_reason = choice
            .finish_reason
            .map(|f| Self::to_native_finish_reason(&f));

        if let Some(text) = &choice.message.content {
            if let Some(cfg) = &config
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
                }
            } else {
                message_parts.push(text.clone());
            }
        }

        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                let mut call;
                match tc {
                    async_openai::types::chat::ChatCompletionMessageToolCalls::Function(f) => {
                        call = Self::to_native_function_call(&f.function).unwrap_or_default();
                        call.context = f.id.clone().into();
                    }
                    async_openai::types::chat::ChatCompletionMessageToolCalls::Custom(c) => {
                        call = crate::FunctionCall {
                            name: c.custom_tool.name.clone(),
                            arguments: serde_json::from_str(&c.custom_tool.input)
                                .unwrap_or_default(),
                            context: c.id.clone().into(),
                        };
                    }
                }

                function_calls.push(call);
            }
        }

        if let Some(audio) = &choice.message.audio {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(audio.data.clone())
                .unwrap_or_default();
            audio_parts.push(decoded);
        }

        if let Some(annotations) = &choice.message.annotations {
            for annotation in annotations {
                match annotation {
                    async_openai::types::chat::ChatCompletionResponseMessageAnnotation::UrlCitation { url_citation } => {
                        thoughts.push(crate::inference::Thought {
                            text: format!("Url Citation: {}", url_citation.url),
                            context: None,
                        });
                        continue;
                    }
                }
            }
        }

        let usage: Option<inference::UsageMetadata> = response.usage.as_ref().map(|u| {
            let cached_tokens = u
                .prompt_tokens_details
                .as_ref()
                .and_then(|b| b.cached_tokens.map(|v| v as i32));

            let thought_tokens = u
                .completion_tokens_details
                .as_ref()
                .and_then(|b| b.reasoning_tokens.map(|v| v as i32));

            inference::UsageMetadata {
                prompt_tokens: Some(u.prompt_tokens as i32),
                thought_tokens,
                cached_tokens,
                total_tokens: Some(u.total_tokens as i32),
            }
        });

        crate::Inference {
            model: Some(response.model.clone()),
            content: crate::prelude::InferenceContent {
                role: Self::to_native_role(&Self::chat_role_to_message_role(&choice.message.role)),
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
            context: serde_json::to_value(&response.id).ok(),
            function_results: vec![], // Currently, we do not extract function results from standard responses.
        }
    }

    fn to_provider_message(
        inference: &crate::Inference,
    ) -> async_openai::types::chat::ChatCompletionRequestMessage {
        let role = Self::to_provider_role(&inference.content.role);

        // Build tool_calls for Assistant
        let tool_calls: Vec<ChatCompletionMessageToolCalls> = inference
            .function_calls
            .iter()
            .filter_map(|fc| {
                Self::to_provider_function_call(fc).ok().map(|function| {
                    ChatCompletionMessageToolCalls::Function(ChatCompletionMessageToolCall {
                        id: fc.context.clone().unwrap_or_default(),
                        function,
                    })
                })
            })
            .collect();

        // Build content_parts
        let mut content_parts: Vec<async_openai::types::responses::MessageContent> = vec![];

        if let Some(text) = &inference.content.text {
            content_parts.push(async_openai::types::responses::MessageContent::Text(
                TextContent { text: text.clone() },
            ));
        }

        for thought in &inference.thoughts {
            content_parts.push(
                async_openai::types::responses::MessageContent::ReasoningText(
                    ReasoningTextContent {
                        text: thought.text.clone(),
                    },
                ),
            );
        }

        match role {
            async_openai::types::responses::MessageRole::Tool => {
                Self::build_tool_message(inference)
            }
            async_openai::types::responses::MessageRole::User => {
                Self::build_user_message(&content_parts)
            }
            async_openai::types::responses::MessageRole::System => {
                Self::build_system_message(&content_parts)
            }
            async_openai::types::responses::MessageRole::Developer => {
                Self::build_developer_message(&content_parts)
            }
            _ => Self::build_assistant_message(&content_parts, tool_calls),
        }
    }

    fn new_chat_request(
        &self,
        _req: super::ChatCompletionRequest,
    ) -> crate::Result<async_openai::types::chat::CreateChatCompletionRequestArgs> {
        let mut history: Vec<async_openai::types::chat::ChatCompletionRequestMessage> = _req
            .messages
            .iter()
            .map(Self::to_provider_message)
            .collect();

        history.push(Self::to_provider_message(_req.inference));

        let mut req = async_openai::types::chat::CreateChatCompletionRequestArgs::default();
        let mut req = req
            .model(_req.model.to_string())
            .max_tokens(_req.config.max_output_tokens as u32)
            .top_p(_req.config.top_p)
            .temperature(_req.config.temperature)
            .reasoning_effort(Self::to_provider_thinking_mode(&_req.config.thinking_mode)?)
            .messages(history.clone());

        // Schema & Tool injection logic
        match (&_req.config.output_schema, _req.config.native_tool_handling) {
            (Some(schema), false) => {
                let wrapped = NonNativeFunctionCallingSchema::new_dynamic_inner(schema.clone());
                req = req.response_format(ResponseFormat::JsonSchema {
                    json_schema: ResponseFormatJsonSchema {
                        name: "response".to_string(),
                        description: Some("Schema for the response".to_string()),
                        schema: Some(wrapped.into()),
                        strict: Some(true),
                    },
                });
            }
            (Some(schema), true) => {
                req = req.response_format(ResponseFormat::JsonSchema {
                    json_schema: ResponseFormatJsonSchema {
                        name: "response".to_string(),
                        description: Some("Schema for the response".to_string()),
                        schema: Some(schema.clone().into()),
                        strict: Some(true),
                    },
                });
            }
            (None, false) => {
                let default_schema = NonNativeFunctionCallingSchema::new_default();
                req = req.response_format(ResponseFormat::JsonSchema {
                    json_schema: ResponseFormatJsonSchema {
                        name: "response".to_string(),
                        description: Some("Schema for the response".to_string()),
                        schema: Some(default_schema.into()),
                        strict: Some(false),
                    },
                });
            }
            (None, true) => {} // No action needed for native tools without explicit output schema
        }

        if let Some(stop_sequences) = &_req.config.stop_sequences {
            req = req.stop(StopConfiguration::StringArray(stop_sequences.clone()));
        }

        let mut system_prompt = _req.system_prompt.to_string();
        if _req.config.native_tool_handling {
            let tool_choice = Self::to_provider_tool_calling_mode(&_req.config.tool_calling_mode);
            req = req.tool_choice(tool_choice);
            req = req.tools(Self::to_chat_completion_tools(_req.tools))
        } else {
            req = req.tool_choice(
                async_openai::types::chat::ChatCompletionToolChoiceOption::Mode(
                    async_openai::types::chat::ToolChoiceOptions::None,
                ),
            );

            system_prompt.push_str(&format!(
                "\n\nThe following tools are available to you:\n{}\n\n",
                crate::util::tools_to_string(_req.tools)
            ));
        }

        Ok(req.clone())
    }
}
