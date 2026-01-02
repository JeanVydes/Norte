use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize, de};
use serde_json::Value;
use std::{fmt::Debug, sync::Arc};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct FunctionCall {
    #[schemars(description = "Name of the function being called")]
    pub name: String,
    #[schemars(
        description = "Arguments provided to the function call should match required arguments by the used tool"
    )]
    pub arguments: Value,
    // this is managed internally, so dont deserialize
    #[serde(skip_deserializing)]
    pub context: Option<String>,
}

impl Default for FunctionCall {
    fn default() -> Self {
        Self {
            name: String::new(),
            arguments: Value::Null,
            context: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct FunctionResult {
    pub name: String,
    pub results: Value,
    pub context: Option<String>,
}

impl FunctionCall {
    pub fn empty_checkpoint() -> Self {
        Self {
            name: "___empty_checkpoint___".to_string(),
            arguments: Value::Null,
            context: None,
        }
    }
}

pub trait ToolArgs: JsonSchema + Serialize + Send + Sync {}

impl<T> ToolArgs for T where T: JsonSchema + Serialize + Send + Sync {}

#[async_trait::async_trait]
pub trait FnExecutor<A, R>: Send + Sync
where
    A: ToolArgs + Send + Sync,
    R: Serialize + Send + Sync,
{
    async fn call(&self, args: A) -> crate::Result<R>;
}

pub trait FnDeclarator<A, R>: Send + Sync
where
    A: ToolArgs + Send + Sync,
    R: Serialize + Send + Sync,
{
    fn declare(&self) -> FunctionDeclaration<A, R>;
}

#[async_trait::async_trait]
pub trait AnyFunction: Send + Debug + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> &Schema;

    async fn execute(&self, args: &serde_json::Value) -> crate::Result<serde_json::Value>;

    // { "type": "function", "function": { "name": "get_time" } }
    fn to_generic_tool(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name(),
                "description": self.description(),
                "parameters": self.parameters_schema(),
            }
        })
    }

    #[cfg(feature = "google")]
    fn to_google(&self) -> gemini_rust::Tool;
}

impl PartialEq for dyn AnyFunction {
    fn eq(&self, other: &Self) -> bool {
        self.name() == other.name()
            && self.description() == other.description()
            && self.parameters_schema() == other.parameters_schema()
    }
}

impl Eq for dyn AnyFunction {}

impl<A, R> From<FunctionDeclaration<A, R>> for Arc<dyn AnyFunction>
where
    A: de::DeserializeOwned + ToolArgs + Debug + 'static,
    R: Serialize + Send + Sync + 'static,
{
    fn from(decl: FunctionDeclaration<A, R>) -> Self {
        Arc::new(decl)
    }
}

#[async_trait::async_trait]
impl<A, R> AnyFunction for FunctionDeclaration<A, R>
where
    A: de::DeserializeOwned + Debug + ToolArgs + 'static,
    R: Serialize + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn parameters_schema(&self) -> &Schema {
        &self.parameters
    }

    async fn execute(&self, args: &serde_json::Value) -> crate::Result<serde_json::Value> {
        let concrete_args: A = serde_json::from_value(args.clone())?;

        let result = self.executor.call(concrete_args).await?;
        Ok(serde_json::to_value(result)?)
    }

    #[cfg(feature = "google")]
    fn to_google(&self) -> gemini_rust::Tool {
        gemini_rust::Tool::Function {
            function_declarations: vec![
                gemini_rust::FunctionDeclaration::new(
                    self.name,
                    self.description,
                    Some(gemini_rust::tools::Behavior::Blocking),
                )
                .with_parameters::<A>()
                .with_response::<serde_json::Value>(),
            ],
        }
    }
}

#[derive(Clone)]
pub struct FunctionDeclaration<A, R>
where
    A: ToolArgs,
    R: Serialize + Send + Sync,
{
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Schema,
    pub executor: Arc<dyn FnExecutor<A, R>>,
}

impl<T, U> FunctionDeclaration<T, U>
where
    T: Debug + ToolArgs + Serialize + de::DeserializeOwned + 'static,
    U: Serialize + Send + Sync + 'static,
{
    pub fn new(
        name: &'static str,
        description: &'static str,
        executor: Arc<dyn FnExecutor<T, U>>,
    ) -> Self {
        let schema = schemars::schema_for!(T);
        Self {
            name,
            description,
            parameters: schema,
            executor,
        }
    }

    pub fn into_any(self) -> Arc<dyn AnyFunction> {
        Arc::new(self)
    }
}

impl<A, R> Debug for FunctionDeclaration<A, R>
where
    A: ToolArgs,
    R: Serialize + Send + Sync,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FunctionDeclaration")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("parameters", &self.parameters)
            .field("executor", &"<executor>")
            .finish()
    }
}

#[macro_export]
macro_rules! declare_function {
    ($name:expr, $description:expr, $args_ty:ty, $result_ty:ty, $executor:expr) => {{
        let schema = schemars::schema_for!($args_ty);
        FunctionDeclaration {
            name: $name,
            description: $description,
            parameters: schema,
            executor: std::sync::Arc::new($executor),
        }
    }};
}
