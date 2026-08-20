//! Thin client for Ollama's `/api/chat` endpoint, including tool-calling.
//!
//! Note: unlike OpenAI, Ollama returns tool-call `arguments` as a JSON object,
//! not a stringified JSON blob, so we carry them as `serde_json::Value`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Set on `role = "tool"` messages so the model knows which tool answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
            tool_calls: None,
            tool_name: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
            tool_calls: None,
            tool_name: None,
        }
    }
    pub fn tool_result(tool_name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            tool_calls: None,
            tool_name: Some(tool_name.into()),
        }
    }
}

/// A tool definition sent to the model. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolDef {
    pub fn function(name: String, description: String, parameters: Value) -> Self {
        Self {
            kind: "function",
            function: FunctionDef {
                name,
                description,
                parameters,
            },
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    tools: &'a [ToolDef],
    stream: bool,
    /// Ollama generation options (temperature, num_ctx, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<&'a Value>,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: Message,
}

pub struct OllamaClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    /// Sent as the request `options`. Defaults to deterministic tool use.
    options: Value,
}

impl OllamaClient {
    /// Create a client. `options` overrides the generation options; when `None`,
    /// a deterministic default (`temperature = 0`) is used so tool-calling is as
    /// reliable as the model allows.
    pub fn new(base_url: String, model: String, options: Option<Value>) -> Self {
        let options = options.unwrap_or_else(|| serde_json::json!({ "temperature": 0 }));
        Self {
            http: reqwest::Client::new(),
            base_url,
            model,
            options,
        }
    }

    /// Send the full conversation plus tool defs and return the assistant turn.
    pub async fn chat(&self, messages: &[Message], tools: &[ToolDef]) -> Result<Message> {
        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
        let body = ChatRequest {
            model: &self.model,
            messages,
            tools,
            stream: false,
            options: Some(&self.options),
        };

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url} (is `ollama serve` running?)"))?;

        let status = resp.status();
        let text = resp.text().await.context("reading Ollama response body")?;
        if !status.is_success() {
            anyhow::bail!("Ollama returned {status}: {text}");
        }

        let parsed: ChatResponse = serde_json::from_str(&text)
            .with_context(|| format!("unexpected Ollama response shape: {text}"))?;
        Ok(parsed.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_carries_options_and_no_stream() {
        let client = OllamaClient::new("http://x".into(), "m".into(), None);
        let body = ChatRequest {
            model: &client.model,
            messages: &[],
            tools: &[],
            stream: false,
            options: Some(&client.options),
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["stream"], false);
        assert_eq!(v["options"]["temperature"], 0);
    }

    #[test]
    fn explicit_options_override_the_default() {
        let client = OllamaClient::new(
            "http://x".into(),
            "m".into(),
            Some(serde_json::json!({ "temperature": 0.7, "num_ctx": 8192 })),
        );
        assert_eq!(client.options["temperature"], 0.7);
        assert_eq!(client.options["num_ctx"], 8192);
    }

    #[test]
    fn tool_result_message_serializes_with_tool_name() {
        let m = Message::tool_result("execute_sql", "rows");
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["tool_name"], "execute_sql");
        assert_eq!(v["content"], "rows");
    }
}
