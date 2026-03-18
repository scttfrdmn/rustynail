use agenkit::{AgentError, Tool, ToolResult};
use async_trait::async_trait;
use std::collections::HashMap;

pub struct CalculatorTool;

#[async_trait]
impl Tool for CalculatorTool {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "Perform mathematical calculations. Supports: add, sub, mul, div, pow, sqrt, abs, floor, ceil, round."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["add", "sub", "mul", "div", "pow", "sqrt", "abs", "floor", "ceil", "round"],
                    "description": "The operation to perform"
                },
                "a": {
                    "type": "number",
                    "description": "First operand"
                },
                "b": {
                    "type": "number",
                    "description": "Second operand (required for add, sub, mul, div, pow)"
                }
            },
            "required": ["op", "a"]
        }))
    }

    async fn execute(
        &self,
        params: HashMap<String, serde_json::Value>,
    ) -> Result<ToolResult, AgentError> {
        let op = match params.get("op").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => return Ok(ToolResult::error("missing required parameter 'op'")),
        };

        let a = match params.get("a").and_then(|v| v.as_f64()) {
            Some(a) => a,
            None => return Ok(ToolResult::error("missing or invalid parameter 'a'")),
        };

        let result = match op {
            "sqrt" => {
                if a < 0.0 {
                    return Ok(ToolResult::error("sqrt of negative number is undefined"));
                }
                a.sqrt()
            }
            "abs" => a.abs(),
            "floor" => a.floor(),
            "ceil" => a.ceil(),
            "round" => a.round(),
            _ => {
                // Binary ops require b
                let b = match params.get("b").and_then(|v| v.as_f64()) {
                    Some(b) => b,
                    None => {
                        return Ok(ToolResult::error(format!(
                            "missing required parameter 'b' for op '{}'",
                            op
                        )))
                    }
                };
                match op {
                    "add" => a + b,
                    "sub" => a - b,
                    "mul" => a * b,
                    "div" => {
                        if b == 0.0 {
                            return Ok(ToolResult::error("division by zero"));
                        }
                        a / b
                    }
                    "pow" => a.powf(b),
                    unknown => {
                        return Ok(ToolResult::error(format!("unknown op '{}'", unknown)))
                    }
                }
            }
        };

        Ok(ToolResult::success(serde_json::json!({ "result": result })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn calc(op: &str, a: f64, b: Option<f64>) -> ToolResult {
        let mut params = HashMap::new();
        params.insert("op".to_string(), serde_json::json!(op));
        params.insert("a".to_string(), serde_json::json!(a));
        if let Some(b) = b {
            params.insert("b".to_string(), serde_json::json!(b));
        }
        CalculatorTool.execute(params).await.unwrap()
    }

    fn result_f64(r: &ToolResult) -> f64 {
        r.output["result"].as_f64().unwrap()
    }

    #[tokio::test]
    async fn test_add() {
        let r = calc("add", 3.0, Some(4.0)).await;
        assert!(r.success);
        assert_eq!(result_f64(&r), 7.0);
    }

    #[tokio::test]
    async fn test_sub() {
        let r = calc("sub", 10.0, Some(3.0)).await;
        assert!(r.success);
        assert_eq!(result_f64(&r), 7.0);
    }

    #[tokio::test]
    async fn test_mul() {
        let r = calc("mul", 3.0, Some(4.0)).await;
        assert!(r.success);
        assert_eq!(result_f64(&r), 12.0);
    }

    #[tokio::test]
    async fn test_div() {
        let r = calc("div", 10.0, Some(2.0)).await;
        assert!(r.success);
        assert_eq!(result_f64(&r), 5.0);
    }

    #[tokio::test]
    async fn test_div_by_zero() {
        let r = calc("div", 10.0, Some(0.0)).await;
        assert!(!r.success);
        assert!(r.error.unwrap().contains("division by zero"));
    }

    #[tokio::test]
    async fn test_pow() {
        let r = calc("pow", 2.0, Some(10.0)).await;
        assert!(r.success);
        assert_eq!(result_f64(&r), 1024.0);
    }

    #[tokio::test]
    async fn test_sqrt() {
        let r = calc("sqrt", 9.0, None).await;
        assert!(r.success);
        assert_eq!(result_f64(&r), 3.0);
    }

    #[tokio::test]
    async fn test_sqrt_negative() {
        let r = calc("sqrt", -1.0, None).await;
        assert!(!r.success);
    }

    #[tokio::test]
    async fn test_abs() {
        let r = calc("abs", -5.0, None).await;
        assert!(r.success);
        assert_eq!(result_f64(&r), 5.0);
    }

    #[tokio::test]
    async fn test_floor() {
        let r = calc("floor", 3.7, None).await;
        assert!(r.success);
        assert_eq!(result_f64(&r), 3.0);
    }

    #[tokio::test]
    async fn test_ceil() {
        let r = calc("ceil", 3.2, None).await;
        assert!(r.success);
        assert_eq!(result_f64(&r), 4.0);
    }

    #[tokio::test]
    async fn test_round() {
        let r = calc("round", 3.5, None).await;
        assert!(r.success);
        assert_eq!(result_f64(&r), 4.0);
    }

    #[tokio::test]
    async fn test_missing_b_for_binary_op() {
        let r = calc("add", 3.0, None).await;
        assert!(!r.success);
        assert!(r.error.unwrap().contains("'b'"));
    }
}
