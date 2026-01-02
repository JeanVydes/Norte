//! units
//!
//! Unit conversion tool without external libraries.
//! Optimized for compiler static sizing requirements.

use rustychain::FunctionDeclaration;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct UnitArgs {
    pub value: f64,
    #[schemars(description = "Source unit (e.g., 'mb', 'kg', 'celsius', 'meters', 'hours')")]
    pub from: String,
    #[schemars(description = "Target unit (e.g., 'gb', 'lb', 'fahrenheit', 'feet', 'minutes')")]
    pub to: String,
}

#[derive(Clone, Default)]
pub struct UnitConverterTool;

impl UnitConverterTool {
    fn convert(&self, val: f64, from: &str, to: &str) -> Option<f64> {
        let from = from.to_lowercase();
        let to = to.to_lowercase();

        if from == to {
            return Some(val);
        }

        // Temperature (Special case: non-multiplicative)
        match (from.as_str(), to.as_str()) {
            ("celsius", "fahrenheit") => return Some((val * 9.0 / 5.0) + 32.0),
            ("fahrenheit", "celsius") => return Some((val - 32.0) * 5.0 / 9.0),
            ("celsius", "kelvin") => return Some(val + 273.15),
            ("kelvin", "celsius") => return Some(val - 273.15),
            ("fahrenheit", "kelvin") => return Some((val - 32.0) * 5.0 / 9.0 + 273.15),
            ("kelvin", "fahrenheit") => return Some((val - 273.15) * 9.0 / 5.0 + 32.0),
            _ => {}
        }

        // Multiplicative Categories defined as separate slices to avoid size mismatch
        let data = [
            ("bytes", 1.0),
            ("kb", 1024.0),
            ("mb", 1048576.0),
            ("gb", 1073741824.0),
            ("tb", 1099511627776.0),
            ("pb", 1125899906842624.0),
        ];

        let length = [
            ("mm", 0.001),
            ("cm", 0.01),
            ("meters", 1.0),
            ("km", 1000.0),
            ("inches", 0.0254),
            ("feet", 0.3048),
            ("yards", 0.9144),
            ("miles", 1609.34),
        ];

        let mass = [
            ("mg", 0.001),
            ("grams", 1.0),
            ("kg", 1000.0),
            ("ton", 1000000.0),
            ("oz", 28.3495),
            ("lb", 453.592),
        ];

        let time = [
            ("ms", 0.001),
            ("seconds", 1.0),
            ("minutes", 60.0),
            ("hours", 3600.0),
            ("days", 86400.0),
            ("weeks", 604800.0),
        ];

        let digital_speed = [
            ("bps", 1.0),
            ("kbps", 1000.0),
            ("mbps", 1000000.0),
            ("gbps", 1000000000.0),
        ];

        // Search in each category
        let all_categories: &[&[(&str, f64)]] = &[&data, &length, &mass, &time, &digital_speed];

        for cat in all_categories {
            let from_factor = cat.iter().find(|(u, _)| *u == from);
            let to_factor = cat.iter().find(|(u, _)| *u == to);

            if let (Some((_, f1)), Some((_, f2))) = (from_factor, to_factor) {
                return Some(val * (f1 / f2));
            }
        }

        None
    }
}

#[async_trait::async_trait]
impl FnExecutor<UnitArgs, f64> for UnitConverterTool {
    async fn call(&self, args: UnitArgs) -> rustychain::Result<f64> {
        self.convert(args.value, &args.from, &args.to)
            .ok_or_else(|| {
                rustychain::Error::Internal(
                    format!(
                        "Unsupported conversion from '{}' to '{}'",
                        args.from, args.to
                    )
                    .into(),
                )
            })
    }
}

impl FnDeclarator<UnitArgs, f64> for UnitConverterTool {
    fn declare(&self) -> FunctionDeclaration<UnitArgs, f64> {
        FunctionDeclaration {
            name: "unit_converter_tool",
            description: "Advanced unit converter. Supports Data (Bytes to PB), Length (Metric/Imperial), Mass, Time, and Digital Speed.",
            parameters: schema_for!(UnitArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
