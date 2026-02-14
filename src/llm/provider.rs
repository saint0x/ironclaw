//! LLM provider trait and types.

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;

/// Role in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// Tool call ID if this is a tool result message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Name of the tool for tool results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Tool calls requested by the assistant (for conversation replay).
    /// OpenAI-compatible APIs require the assistant message to include
    /// tool_calls when followed by tool result messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessage {
    /// Create a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
            name: None,
            tool_calls: None,
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            name: None,
            tool_calls: None,
        }
    }

    /// Create an assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            name: None,
            tool_calls: None,
        }
    }

    /// Create an assistant message that requested tool calls.
    ///
    /// OpenAI-compatible APIs require the assistant message to carry the
    /// `tool_calls` array when followed by tool-result messages.
    pub fn assistant_with_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            name: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        }
    }

    /// Create a tool result message.
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
            tool_calls: None,
        }
    }
}

/// Request for a chat completion.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub messages: Vec<ChatMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub stop_sequences: Option<Vec<String>>,
}

impl CompletionRequest {
    /// Create a new completion request.
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
        }
    }

    /// Set max tokens.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Set temperature.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }
}

/// Response from a chat completion.
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    pub content: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub finish_reason: FinishReason,
    /// Tokens read from prompt cache (Anthropic).
    pub cache_read_tokens: Option<u32>,
    /// Tokens written to prompt cache.
    pub cache_write_tokens: Option<u32>,
    /// Tokens consumed by extended thinking.
    pub thinking_tokens: Option<u32>,
    /// Extended thinking content (if model supports it).
    pub thinking: Option<String>,
}

/// Why the completion finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolUse,
    ContentFilter,
    Unknown,
}

impl std::fmt::Display for FinishReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FinishReason::Stop => write!(f, "stop"),
            FinishReason::Length => write!(f, "length"),
            FinishReason::ToolUse => write!(f, "tool_use"),
            FinishReason::ContentFilter => write!(f, "content_filter"),
            FinishReason::Unknown => write!(f, "unknown"),
        }
    }
}

/// A streaming event from the LLM during generation.
///
/// Maps to the Anthropic Messages API streaming events. These are emitted
/// via the AgentEventBus for real-time UI updates.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// New assistant message started.
    MessageStart { message_id: String, model: String },

    /// A new content block started.
    ContentBlockStart {
        index: usize,
        block_type: StreamBlockType,
    },

    /// Streaming delta for a content block.
    ContentBlockDelta { index: usize, delta: StreamDelta },

    /// A content block finished.
    ContentBlockStop { index: usize },

    /// Message-level usage update.
    MessageDelta {
        output_tokens: u32,
        stop_reason: Option<FinishReason>,
    },

    /// Message generation finished.
    MessageStop,
}

/// Type of content block in the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamBlockType {
    Text,
    ToolUse { id: String, name: String },
    Thinking,
}

/// Delta content within a streaming content block.
#[derive(Debug, Clone)]
pub enum StreamDelta {
    /// Text content delta.
    Text(String),
    /// Tool use input JSON delta (streamed JSON args).
    InputJson(String),
    /// Extended thinking delta.
    Thinking(String),
}

/// Definition of a tool for the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Result of a tool execution to send back to the LLM.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

/// Request for a completion with tool use.
#[derive(Debug, Clone)]
pub struct ToolCompletionRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// How to handle tool use: "auto", "required", or "none".
    pub tool_choice: Option<String>,
}

impl ToolCompletionRequest {
    /// Create a new tool completion request.
    pub fn new(messages: Vec<ChatMessage>, tools: Vec<ToolDefinition>) -> Self {
        Self {
            messages,
            tools,
            max_tokens: None,
            temperature: None,
            tool_choice: None,
        }
    }

    /// Set max tokens.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Set temperature.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Set tool choice mode.
    pub fn with_tool_choice(mut self, choice: impl Into<String>) -> Self {
        self.tool_choice = Some(choice.into());
        self
    }
}

/// Response from a completion with potential tool calls.
#[derive(Debug, Clone)]
pub struct ToolCompletionResponse {
    /// Text content (may be empty if tool calls are present).
    pub content: Option<String>,
    /// Tool calls requested by the model.
    pub tool_calls: Vec<ToolCall>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub finish_reason: FinishReason,
    /// Tokens read from prompt cache.
    pub cache_read_tokens: Option<u32>,
    /// Tokens written to prompt cache.
    pub cache_write_tokens: Option<u32>,
    /// Tokens consumed by extended thinking.
    pub thinking_tokens: Option<u32>,
    /// Extended thinking content (if model supports it).
    pub thinking: Option<String>,
}

/// Trait for LLM providers.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Get the model name.
    fn model_name(&self) -> &str;

    /// Get cost per token (input, output).
    fn cost_per_token(&self) -> (Decimal, Decimal);

    /// Complete a chat conversation.
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError>;

    /// Complete with tool use support.
    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError>;

    /// List available models from the provider.
    /// Default implementation returns empty list.
    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        Ok(Vec::new())
    }

    /// Calculate cost for a completion.
    fn calculate_cost(&self, input_tokens: u32, output_tokens: u32) -> Decimal {
        let (input_cost, output_cost) = self.cost_per_token();
        input_cost * Decimal::from(input_tokens) + output_cost * Decimal::from(output_tokens)
    }
}
