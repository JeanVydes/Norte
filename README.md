# RustyChain

A high-performance, type-safe Rust library for building LLM-powered applications, chains, and agents.

## Installation

Add RustyChain to your `Cargo.toml`:

```toml
[dependencies]
rustychain = { git = "https://github.com/JeanVydes/rustychain", package = "rustychain", branch = "dev", features = ["full"] }

# if you want to use the tools suite
#rustychain-tools-suite = { git = "https://github.com/JeanVydes/rustychain", package = "rustychain-tools-suite", branch = "dev" }
``` 

## Start

```rust
use rustychain::{LLM, LLMProvider};

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let llm = LLM::builder()
        .set_authorization("your_api_key_here")
        .set_model("gemini-2.5-flash")
        .set_provider(LLMProvider::Google)
        .set_system_prompt("You are a helpful assistant.")
        .build()?;

    let response = llm.inference("Hello, how are you?").generate().await?;

    println!("{}", response.content.text.unwrap_or_default());

    Ok(())
}
```

## Providers

RustyChain provide a layer to implement tool usage for models that DO NOT support tool calling (but also can be used for models that support tool calling).

We do this by:
1. The tools saved for the models are injected into the system prompt.
2. The requests uses a schema, usually you can also implement a custom schema if needed. But YOUR schema will be a subset of the NATIVE schema of the model.

The following schemas are used internally to achieve this:

```rust
// This is the Schema used when YOU provide a custom schema AND llm generation config is set to use not used native function calling
// T is your custom schema type
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NonNativeFunctionCallingSchema<T> {
    #[schemars(description = "List of function calls made by the model")]
    pub function_calls: Vec<FunctionCall>,
    #[schemars(description = "The original response from the model to the user")]
    pub content: T,
}

// If you do not provide a custom schema, this is the default schema used for non-native function calling
// The content is just a string
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StringSchema {
    #[schemars(description = "List of function calls made by the model")]
    pub function_calls: Vec<FunctionCall>,
    #[schemars(description = "The original response from the model to the user")]
    pub content: String,
}
```
3. The models that natively support function calling can use an custom schema or not
4. The models that do not natively support function calling will use the `NonNativeFunctionCallingSchema` + your custom schema or `StringSchema` if no custom schema is provided.
5. Then, we internally parse the responses and extract the function calls and results.
6. And set to Inference.content.text the normal text response (`NonNativeFunctionCallingSchema.content` or `StringSchema.content`) and set the function calls and results to `Inference.function_calls` and `Inference.function_results`.

If a model do not support function calling natively, you have to set manually the `native_tool_handling` to false in the generation config. The default is `true` because most frontier models now support function calling natively.

```rust
let llm = LLM::builder()....build()?;

// you set the config for an inference, not for the LLM itself
llm.inference("Hello, how are you?")
    .with_config(GenerationConfig::default().with_native_tool_handling(false))
    .generate()
    .await?;

```

|Provider|Chat Completion|Streaming|Native Function Calling|Function Calling Layer|Embeddings|
|--------|----------------|---------|----------------------|----------------------|----------|
|Google  |✅|✅|Depends on model|✅|✅|
|OpenAI  |✅|✅|Depends on model|✅|✅|
|Ollama  |✅|✅|Depends on model|✅|✅|
|OpenRouter|✅|✅|Depends on model|✅|✅|
|DeepSeek|✅|✅|Depends on model|✅|✅|
|Groq|✅|✅|Depends on model|✅|✅|
|Together|✅|✅|Depends on model|✅|✅|
|Mistral|✅|✅|Depends on model|✅|✅|
|Perplexity|✅|✅|Depends on model|✅|✅|
|Fireworks|✅|✅|Depends on model|✅|✅|
|XAI|✅|✅|Depends on model|✅|✅|
|LMStudio|✅|✅|Depends on model|✅|✅|
|Poe|✅|✅|Depends on model|✅|✅|
|Any OpenAI compatible API (set base url for ones not listed here)|✅|✅|Depends on model|✅|✅|
|Anthropic|❌|❌|❌|❌|❌|

## Tools

RustyChain provides a simple way to define and use tools in your LLM applications. Tools can be functions that the LLM can call to perform specific tasks.

```rust
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
pub struct SumArgs {
    #[schemars(description = "First integer to sum.")]
    pub a: i64,
    #[schemars(description = "Second integer to sum.")]
    pub b: i64,
}

#[declare_function(
    name = "sum_integers",
    description = "Returns the sum of two integers.",
    args = SumArgs,
    result = i64
)]
pub struct SumTool {}

impl SumTool {
    pub async fn execute(&self, args: SumArgs) -> rustychain::Result<i64> {
        Ok(args.a + args.b)
    }
}

// you pass the declaration of the tool to the LLM with .declare()
let llm = LLM::builder().add_tool(SumTool {}.declare());
```

MIT License - see [LICENSE](LICENSE) for details.

## Contributing

Contributions welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) first.

## Documentation

Under construction.

---

Built with ❤️
