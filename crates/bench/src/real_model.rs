//! A real, HTTP-backed `Model` implementation against any OpenAI-
//! compatible `/chat/completions` endpoint -- DeepSeek, Kimi/Moonshot,
//! and GLM/Zhipu all expose one, so pointing this at any of the three
//! (or any other provider that speaks the same wire format) is a matter
//! of setting the base URL, not writing a new client.
//!
//! Configured entirely through environment variables (no CLI flags) so
//! `--mode real` in `main.rs` stays a one-word switch:
//!
//! - `NIRDOSHA_BENCH_API_KEY` (required) -- sent as `Authorization:
//!   Bearer <key>`. No key, no request: `RealModel::from_env` returns
//!   `Err` rather than silently falling back to a mock.
//! - `NIRDOSHA_BENCH_API_BASE` (optional, default `DEFAULT_API_BASE`) --
//!   the API root; `/chat/completions` is appended to it.
//! - `NIRDOSHA_BENCH_MODEL` (optional, default `DEFAULT_MODEL`) -- the
//!   `model` field in the request body.
//!
//! Request/response shapes (`ChatCompletionRequest`, `ChatCompletionResponse`)
//! are exercised by real unit tests below that don't need a live key --
//! they assert the JSON this crate sends and the text it extracts from a
//! hand-written sample response, not that any particular provider is
//! reachable right now.

use serde::{Deserialize, Serialize};

use crate::{Model, Task};

pub const API_KEY_ENV: &str = "NIRDOSHA_BENCH_API_KEY";
pub const API_BASE_ENV: &str = "NIRDOSHA_BENCH_API_BASE";
pub const MODEL_ENV: &str = "NIRDOSHA_BENCH_MODEL";

/// DeepSeek's own base URL -- picked as the default only because it's
/// the provider this environment already confirmed is network-reachable
/// (`curl -sI https://api.deepseek.com` -> a real HTTP 401, i.e. TLS +
/// routing work, just no key). Pointing at Kimi/Moonshot or GLM/Zhipu
/// instead is just setting `NIRDOSHA_BENCH_API_BASE` (and usually
/// `NIRDOSHA_BENCH_MODEL`) to that provider's own values.
pub const DEFAULT_API_BASE: &str = "https://api.deepseek.com";
pub const DEFAULT_MODEL: &str = "deepseek-chat";

const SYSTEM_PROMPT: &str = "You are an expert Nirdosha programmer. Given a task description, respond with ONLY a complete, compilable Nirdosha program that solves it -- no prose, no explanation, no markdown code fences.";

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: f64,
}

impl ChatCompletionRequest {
    /// Builds the request for one generation attempt. On the first
    /// attempt (`prior_failure: None`) this is a plain two-message
    /// exchange (system + the task prompt). On a retry, a third `user`
    /// message carries the previous attempt's structured diagnostic (or
    /// plain-text parse/lex error) back to the model, asking for a fix
    /// -- the actual re-prompt-with-diagnostics mechanism this harness
    /// exists to exercise (see `lib.rs`'s module doc).
    pub fn for_task(model: &str, task_prompt: &str, prior_failure: Option<&str>) -> Self {
        let mut messages = vec![
            ChatMessage { role: "system".to_string(), content: SYSTEM_PROMPT.to_string() },
            ChatMessage { role: "user".to_string(), content: task_prompt.to_string() },
        ];
        if let Some(failure) = prior_failure {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: format!(
                    "The previous program failed with this diagnostic:\n\n{failure}\n\nFix it and reply with the corrected Nirdosha program only, no prose, no markdown code fences."
                ),
            });
        }
        ChatCompletionRequest { model: model.to_string(), messages, temperature: 0.0 }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct ChatCompletionResponse {
    pub choices: Vec<ChatChoice>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct ChatChoice {
    pub message: ChatResponseMessage,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct ChatResponseMessage {
    pub content: String,
}

impl ChatCompletionResponse {
    /// The completion text of the first choice, with a wrapping
    /// markdown code fence stripped if the model added one despite the
    /// system prompt asking it not to -- real models do this often
    /// enough that defending against it here (once) beats every call
    /// site re-deriving the same strip.
    pub fn completion_text(&self) -> Option<String> {
        self.choices.first().map(|c| strip_code_fence(&c.message.content))
    }
}

/// Strips a single leading/trailing ``` ```` fence (with an optional
/// language tag on the opening line, e.g. ` ```nirdosha`), if present.
/// Text with no fence is returned unchanged (trimmed).
fn strip_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed.to_string();
    };
    let after_open = match after_open.find('\n') {
        Some(i) => &after_open[i + 1..],
        None => after_open,
    };
    match after_open.rfind("```") {
        Some(i) => after_open[..i].trim().to_string(),
        None => after_open.trim().to_string(),
    }
}

/// An OpenAI-compatible chat-completions `Model`. Construct via
/// `from_env` -- there is no `Default`, since a client with no API key
/// isn't a valid one.
pub struct RealModel {
    client: reqwest::blocking::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl RealModel {
    /// Reads `NIRDOSHA_BENCH_API_KEY`/`_API_BASE`/`_MODEL` from the
    /// environment. Returns `Err` with a message naming the missing
    /// variable (and the env vars available to configure it) if the API
    /// key isn't set -- callers must not treat that as "fall back to a
    /// mock," only as a hard error to surface to the user.
    pub fn from_env() -> Result<Self, String> {
        let api_key = std::env::var(API_KEY_ENV).map_err(|_| {
            format!(
                "{API_KEY_ENV} is not set. `--mode real` needs an API key for an OpenAI-compatible \
                 chat-completions endpoint (DeepSeek, Kimi/Moonshot, GLM/Zhipu, ...). Set {API_KEY_ENV}, \
                 and optionally {API_BASE_ENV} (default: {DEFAULT_API_BASE}) and {MODEL_ENV} (default: {DEFAULT_MODEL})."
            )
        })?;
        let base_url = std::env::var(API_BASE_ENV).unwrap_or_else(|_| DEFAULT_API_BASE.to_string());
        let model = std::env::var(MODEL_ENV).unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(RealModel { client, base_url, api_key, model })
    }

    fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

impl Model for RealModel {
    fn generate(&mut self, task: &Task, prior_failure: Option<&str>) -> String {
        let request = ChatCompletionRequest::for_task(&self.model, &task.prompt, prior_failure);
        let url = self.chat_completions_url();
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .unwrap_or_else(|e| panic!("request to {url} failed: {e}"));
        let status = response.status();
        let body = response.text().unwrap_or_else(|e| panic!("failed to read response body from {url}: {e}"));
        if !status.is_success() {
            panic!("{url} returned HTTP {status}: {body}");
        }
        let parsed: ChatCompletionResponse = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("failed to parse chat-completions response from {url}: {e}\nbody was: {body}"));
        parsed
            .completion_text()
            .unwrap_or_else(|| panic!("chat-completions response from {url} had no choices: {body}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_json_shape_first_attempt() {
        let req = ChatCompletionRequest::for_task("deepseek-chat", "write a program that returns 42", None);
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "deepseek-chat");
        assert_eq!(json["temperature"], 0.0);
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2, "no prior_failure -- expect system + user only");
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "write a program that returns 42");
    }

    #[test]
    fn request_json_shape_retry_carries_prior_failure() {
        let req = ChatCompletionRequest::for_task("deepseek-chat", "write a program that returns 42", Some("parse error: unexpected token"));
        let json = serde_json::to_value(&req).unwrap();
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3, "prior_failure -- expect a third message");
        assert_eq!(messages[2]["role"], "user");
        let content = messages[2]["content"].as_str().unwrap();
        assert!(content.contains("parse error: unexpected token"), "retry message should carry the diagnostic verbatim, got: {content}");
    }

    #[test]
    fn response_parses_completion_text() {
        let sample = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "choices": [
                {
                    "index": 0,
                    "message": { "role": "assistant", "content": "fn main() -> i64 { return 42 }" },
                    "finish_reason": "stop"
                }
            ]
        }"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(sample).unwrap();
        assert_eq!(parsed.completion_text().as_deref(), Some("fn main() -> i64 { return 42 }"));
    }

    #[test]
    fn response_strips_markdown_code_fence_with_language_tag() {
        let sample = r#"{"choices": [{"message": {"role": "assistant", "content": "```nirdosha\nfn main() -> i64 { return 42 }\n```"}}]}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(sample).unwrap();
        assert_eq!(parsed.completion_text().as_deref(), Some("fn main() -> i64 { return 42 }"));
    }

    #[test]
    fn response_strips_bare_code_fence() {
        let sample = r#"{"choices": [{"message": {"role": "assistant", "content": "```\nfn main() -> i64 { return 1 }\n```"}}]}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(sample).unwrap();
        assert_eq!(parsed.completion_text().as_deref(), Some("fn main() -> i64 { return 1 }"));
    }

    #[test]
    fn response_with_no_choices_yields_none() {
        let sample = r#"{"choices": []}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(sample).unwrap();
        assert_eq!(parsed.completion_text(), None);
    }

    #[test]
    fn from_env_errors_clearly_without_falling_back_to_mock_when_key_is_missing() {
        // SAFETY (single-threaded assumption): these three env vars are
        // private to this test and not read by any other test in this
        // binary, so mutating them here doesn't race other `#[test]`s.
        // Cleared/restored around the assertion so this test doesn't
        // leak state into others in the same process regardless of
        // whether the developer's own shell happens to export the key.
        let saved = std::env::var(API_KEY_ENV).ok();
        unsafe { std::env::remove_var(API_KEY_ENV) };
        let message = match RealModel::from_env() {
            Err(message) => message,
            Ok(_) => panic!("expected Err when NIRDOSHA_BENCH_API_KEY is unset"),
        };
        assert!(message.contains(API_KEY_ENV), "error should name the missing env var, got: {message}");
        if let Some(value) = saved {
            unsafe { std::env::set_var(API_KEY_ENV, value) };
        }
    }
}
