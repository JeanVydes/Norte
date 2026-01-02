use rustychain::chain::Chain;
use rustychain::chain::parallel::ParallelRunnable;
use rustychain::prelude::*;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Input {
    text: String,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub parts: Vec<String>,
}

#[runnable(
    input = Input,
    output = String,
)]
pub struct Summarizer {
    llm: Arc<LLM>,
}

impl Summarizer {
    pub async fn execute(&self, input: Input) -> rustychain::Result<String> {
        let prompt = format!(
            "Summarize the following text in a concise manner:\n\n{}",
            input.text
        );

        let response = self
            .llm
            .inference(Inference::as_user(prompt))
            .generate()
            .await?
            .content
            .text
            .ok_or_else(|| rustychain::Error::NoContent)?;

        Ok(response)
    }
}

#[runnable(
    input = String,
    output = Output,
)]
pub struct BulletPointFormatter {}

impl BulletPointFormatter {
    pub async fn execute(&self, input: String) -> rustychain::Result<Output> {
        let parts: Vec<String> = input
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        Ok(Output { parts })
    }
}

#[runnable(
    input = String,
    output = Output,
)]
pub struct Formatter {}

impl Formatter {
    pub async fn execute(&self, input: String) -> rustychain::Result<Output> {
        let parts: Vec<String> = input
            .to_lowercase()
            .split(' ')
            .map(|s| s.trim().to_string())
            .collect();
        Ok(Output { parts })
    }
}

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    // Initialize the LLM
    let llm = Arc::new(
        LLM::builder()
            .set_authorization("your_api_key_here")
            .set_model("gemini-2.5-flash".to_owned())
            .set_provider(LLMProvider::Google)
            .set_system_prompt("You are a helpful assistant.".to_owned())
            .build()?,
    );

    let formatters = ParallelRunnable::new(BulletPointFormatter {}, Formatter {});

    // Build a chain with two steps: Summarization and Formatting
    let mut chain = Chain::new()
        .add_step("summarization".to_owned(), Summarizer { llm })
        .add_step("formatting".to_owned(), formatters)
        .set_input(Input {
            text: EXAMPLE_TEST.to_owned(),
        });

    // You can also inspect each step's result as they are processed
    while let Some(r) = chain.next().await {
        let step_result = r?;
        match step_result.name.as_str() {
            "summarization" => {
                let _step_result = step_result.downcast::<Input, String>()?;
            }
            "formatting" => {
                let _step_result = step_result.downcast::<String, Output>()?;
            }
            _ => {}
        };
    }

    // Finalize the chain and get the output of the last step
    let _final_output = chain.finalize()?;

    return Ok(());
}

const EXAMPLE_TEST: &str = r#"
Calculus Basics: Derivatives and Integrals

Calculus is fundamentally the mathematical study of change, motion, and accumulation. It is divided into two core concepts: differentiation (derivatives) and integration (integrals). These two concepts are inversely related, much like addition and subtraction.
1. Derivatives

A derivative is a tool used to determine the instantaneous rate of change of a function with respect to one of its variables. It answers the question, "How fast is this changing right now?" Geometrically, calculating the derivative of a function at a specific point gives the slope of the tangent line to the curve at that point . For someone just learning, the simplest analogy is a car's speedometer. If a function represents the distance a car has traveled over time, the derivative of that function is the speedometer's reading—the exact speed at that precise moment in time. It measures the change in distance over an infinitesimally small change in time.
2. Integrals

An integral is the opposite process, known as anti-differentiation. It is a method for summing up small slices of information to find the total accumulation or the net effect over a given interval. Most commonly, an integral is used to calculate the area under a curve . The analogy here is the car's odometer. If the function you are integrating represents the car's speed over time, the integral of that function will give you the total distance traveled from the start of the trip to the end. It accumulates all the small contributions of speed over the entire duration to find a total amount.

In essence, derivatives break down a problem to find the rate of change at a moment, while integrals build up small pieces to find the total accumulation over a period.
"#;
