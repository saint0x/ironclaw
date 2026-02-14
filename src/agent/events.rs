//! Agent event system for real-time execution visibility.
//!
//! Provides a typed event bus that broadcasts agent lifecycle, tool execution,
//! streaming content, and usage metadata to all connected UI consumers
//! (SSE, WebSocket, TUI).
//!
//! Event streams:
//! - **Lifecycle**: Turn start/end, session events, model info
//! - **Tool**: Tool execution start → progress → result with timing
//! - **Assistant**: Streaming text deltas from the LLM
//! - **Thinking**: Extended thinking / reasoning content
//! - **Error**: Execution errors and warnings

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Content blocks — typed content produced by the LLM
// ---------------------------------------------------------------------------

/// A typed content block from an LLM response.
///
/// Maps to the Anthropic Messages API content block types. The UI renders
/// each block type differently (text is displayed, tool_use triggers the
/// tool execution panel, thinking is shown in a collapsible section).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    /// Plain text content.
    #[serde(rename = "text")]
    Text { text: String },

    /// Extended thinking / reasoning (Claude).
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Tool call requested by the model.
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    /// Tool result fed back to the model.
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

// ---------------------------------------------------------------------------
// Usage tracking — per-turn token/cost accounting
// ---------------------------------------------------------------------------

/// Aggregated token usage for a single turn (may span multiple LLM calls).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnUsage {
    /// Input/prompt tokens.
    pub input_tokens: u32,
    /// Output/completion tokens.
    pub output_tokens: u32,
    /// Tokens read from cache (Anthropic prompt caching).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u32>,
    /// Tokens written to cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u32>,
    /// Tokens consumed by extended thinking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_tokens: Option<u32>,
    /// Combined total.
    pub total_tokens: u32,
    /// Model used for this turn (after any fallback).
    pub model: String,
    /// Provider used for this turn.
    pub provider: String,
    /// Estimated cost in USD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<Decimal>,
    /// Wall-clock duration of the turn in milliseconds.
    pub duration_ms: u64,
}

impl TurnUsage {
    /// Add usage from a single LLM call into the aggregate.
    pub fn add_llm_call(&mut self, input: u32, output: u32) {
        self.input_tokens += input;
        self.output_tokens += output;
        self.total_tokens = self.input_tokens + self.output_tokens;
    }
}

// ---------------------------------------------------------------------------
// Tool execution metadata
// ---------------------------------------------------------------------------

/// Execution phase of a tool call, emitted as events for UI rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase")]
pub enum ToolPhase {
    /// Tool execution is about to start.
    #[serde(rename = "start")]
    Start {
        tool_call_id: String,
        name: String,
        arguments: serde_json::Value,
    },

    /// Streaming partial input (JSON args being received from LLM).
    #[serde(rename = "input_delta")]
    InputDelta {
        tool_call_id: String,
        partial_json: String,
    },

    /// Intermediate output from the tool during execution.
    #[serde(rename = "progress")]
    Progress {
        tool_call_id: String,
        name: String,
        partial_result: String,
    },

    /// Tool execution completed.
    #[serde(rename = "result")]
    Result {
        tool_call_id: String,
        name: String,
        result: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        duration_ms: u64,
    },
}

/// Rich metadata for a tool call within a turn (persisted on Turn).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallMeta {
    /// Tool call ID from the LLM.
    pub tool_call_id: String,
    /// Tool name.
    pub name: String,
    /// Arguments passed.
    pub arguments: serde_json::Value,
    /// Result text (if completed).
    pub result: Option<String>,
    /// Error message (if failed).
    pub error: Option<String>,
    /// Execution duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// When execution started.
    pub started_at: DateTime<Utc>,
    /// When execution completed.
    pub completed_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Agent event — the core broadcast type
// ---------------------------------------------------------------------------

/// Classification of event streams.
///
/// Each stream maps to a rendering surface in the UI:
/// - Lifecycle → status bar, turn headers
/// - Tool → tool execution panels
/// - Assistant → chat bubble (streaming text)
/// - Thinking → collapsible reasoning section
/// - Error → error toasts / inline errors
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStream {
    Lifecycle,
    Tool,
    Assistant,
    Thinking,
    Error,
}

/// An event emitted during agent execution.
///
/// Events are broadcast to all connected consumers (SSE, WebSocket, TUI)
/// in real-time. Each event carries a monotonic sequence number per run
/// to guarantee ordering on the client side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    /// Run ID (maps to a single agent turn / prompt call).
    pub run_id: String,
    /// Monotonic sequence number within the run.
    pub seq: u64,
    /// Timestamp (milliseconds since epoch).
    pub ts: i64,
    /// Which stream this event belongs to.
    pub stream: EventStream,
    /// Event payload.
    pub data: EventData,
    /// Session key for routing (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
}

/// Typed event payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum EventData {
    // -- Lifecycle --
    /// A new turn has started.
    #[serde(rename = "turn_start")]
    TurnStart {
        turn_number: usize,
        thread_id: Uuid,
        model: String,
        provider: String,
    },

    /// A turn has completed.
    #[serde(rename = "turn_end")]
    TurnEnd {
        turn_number: usize,
        thread_id: Uuid,
        usage: TurnUsage,
        content_blocks: Vec<ContentBlock>,
        stop_reason: String,
    },

    /// Context compaction was performed.
    #[serde(rename = "compaction")]
    Compaction {
        tokens_before: u32,
        tokens_after: u32,
        turns_removed: usize,
    },

    // -- Assistant --
    /// Streaming text delta from the LLM.
    #[serde(rename = "text_delta")]
    TextDelta { delta: String, text: String },

    // -- Thinking --
    /// Streaming thinking delta.
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { delta: String },

    // -- Tool --
    /// Tool execution event (start, progress, result).
    #[serde(rename = "tool")]
    Tool { phase: ToolPhase },

    /// Tool requires user approval.
    #[serde(rename = "approval_needed")]
    ApprovalNeeded {
        request_id: String,
        tool_name: String,
        description: String,
        parameters: serde_json::Value,
    },

    // -- Error --
    /// An error occurred during execution.
    #[serde(rename = "error")]
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        recoverable: Option<bool>,
    },
}

// ---------------------------------------------------------------------------
// Event bus — broadcast channel with per-run sequence tracking
// ---------------------------------------------------------------------------

/// Broadcasts agent events to all connected consumers.
///
/// Uses a tokio broadcast channel internally. Each run gets monotonically
/// increasing sequence numbers so clients can detect gaps and reorder.
pub struct AgentEventBus {
    tx: broadcast::Sender<AgentEvent>,
    /// Per-run sequence counters.
    seq_counters: dashmap::DashMap<String, Arc<AtomicU64>>,
}

impl AgentEventBus {
    /// Create a new event bus with the given buffer capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            tx,
            seq_counters: dashmap::DashMap::new(),
        }
    }

    /// Emit an event, automatically assigning sequence number and timestamp.
    pub fn emit(&self, run_id: &str, stream: EventStream, data: EventData) {
        self.emit_with_session(run_id, stream, data, None);
    }

    /// Emit an event with an optional session key.
    pub fn emit_with_session(
        &self,
        run_id: &str,
        stream: EventStream,
        data: EventData,
        session_key: Option<String>,
    ) {
        let counter = self
            .seq_counters
            .entry(run_id.to_string())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)));
        let seq = counter.fetch_add(1, Ordering::Relaxed) + 1;

        let event = AgentEvent {
            run_id: run_id.to_string(),
            seq,
            ts: Utc::now().timestamp_millis(),
            stream,
            data,
            session_key,
        };

        let _ = self.tx.send(event);
    }

    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.tx.subscribe()
    }

    /// Clean up sequence counter for a completed run.
    pub fn finish_run(&self, run_id: &str) {
        self.seq_counters.remove(run_id);
    }

    /// Get the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for AgentEventBus {
    fn default() -> Self {
        Self::new(512)
    }
}

// ---------------------------------------------------------------------------
// Turn result — aggregated metadata from a completed turn
// ---------------------------------------------------------------------------

/// Complete result of an agent turn, persisted on the Turn struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnResult {
    /// Whether the turn was aborted externally.
    pub aborted: bool,
    /// Whether the turn timed out.
    pub timed_out: bool,
    /// All assistant text segments produced.
    pub assistant_texts: Vec<String>,
    /// Rich tool call metadata (with timing).
    pub tool_calls: Vec<ToolCallMeta>,
    /// Content blocks produced by the LLM.
    pub content_blocks: Vec<ContentBlock>,
    /// Aggregated usage for this turn.
    pub usage: TurnUsage,
    /// Why the model stopped generating.
    pub stop_reason: StopReason,
    /// Total turn duration in milliseconds.
    pub duration_ms: u64,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Natural end of response.
    EndTurn,
    /// Max tokens reached.
    MaxTokens,
    /// Model wants to call tools.
    ToolUse,
    /// Content filter triggered.
    ContentFilter,
    /// External abort signal.
    Aborted,
    /// Timeout.
    Timeout,
    /// Unknown or unrecognized.
    Unknown,
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StopReason::EndTurn => write!(f, "end_turn"),
            StopReason::MaxTokens => write!(f, "max_tokens"),
            StopReason::ToolUse => write!(f, "tool_use"),
            StopReason::ContentFilter => write!(f, "content_filter"),
            StopReason::Aborted => write!(f, "aborted"),
            StopReason::Timeout => write!(f, "timeout"),
            StopReason::Unknown => write!(f, "unknown"),
        }
    }
}

impl From<crate::llm::FinishReason> for StopReason {
    fn from(reason: crate::llm::FinishReason) -> Self {
        match reason {
            crate::llm::FinishReason::Stop => StopReason::EndTurn,
            crate::llm::FinishReason::Length => StopReason::MaxTokens,
            crate::llm::FinishReason::ToolUse => StopReason::ToolUse,
            crate::llm::FinishReason::ContentFilter => StopReason::ContentFilter,
            crate::llm::FinishReason::Unknown => StopReason::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_block_text_serialization() {
        let block = ContentBlock::Text {
            text: "Hello world".into(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("Hello world"));

        let restored: ContentBlock = serde_json::from_str(&json).unwrap();
        match restored {
            ContentBlock::Text { text } => assert_eq!(text, "Hello world"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn content_block_thinking_serialization() {
        let block = ContentBlock::Thinking {
            thinking: "Let me reason...".into(),
            signature: Some("sig123".into()),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"thinking\""));
        assert!(json.contains("sig123"));
    }

    #[test]
    fn content_block_tool_use_serialization() {
        let block = ContentBlock::ToolUse {
            id: "toolu_abc".into(),
            name: "exec".into(),
            input: serde_json::json!({"cmd": "ls"}),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"tool_use\""));
        assert!(json.contains("toolu_abc"));
    }

    #[test]
    fn tool_phase_start_serialization() {
        let phase = ToolPhase::Start {
            tool_call_id: "tc1".into(),
            name: "exec".into(),
            arguments: serde_json::json!({"cmd": "ls -la"}),
        };
        let json = serde_json::to_string(&phase).unwrap();
        assert!(json.contains("\"phase\":\"start\""));
        assert!(json.contains("exec"));
    }

    #[test]
    fn tool_phase_result_serialization() {
        let phase = ToolPhase::Result {
            tool_call_id: "tc1".into(),
            name: "exec".into(),
            result: "file1.txt\nfile2.txt".into(),
            error: None,
            duration_ms: 42,
        };
        let json = serde_json::to_string(&phase).unwrap();
        assert!(json.contains("\"phase\":\"result\""));
        assert!(json.contains("42"));
    }

    #[test]
    fn turn_usage_add_llm_call() {
        let mut usage = TurnUsage {
            model: "claude-4".into(),
            provider: "anthropic".into(),
            ..Default::default()
        };
        usage.add_llm_call(100, 50);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.total_tokens, 150);

        usage.add_llm_call(200, 100);
        assert_eq!(usage.input_tokens, 300);
        assert_eq!(usage.output_tokens, 150);
        assert_eq!(usage.total_tokens, 450);
    }

    #[test]
    fn stop_reason_from_finish_reason() {
        use crate::llm::FinishReason;
        assert_eq!(StopReason::from(FinishReason::Stop), StopReason::EndTurn);
        assert_eq!(
            StopReason::from(FinishReason::Length),
            StopReason::MaxTokens
        );
        assert_eq!(StopReason::from(FinishReason::ToolUse), StopReason::ToolUse);
        assert_eq!(
            StopReason::from(FinishReason::ContentFilter),
            StopReason::ContentFilter
        );
        assert_eq!(StopReason::from(FinishReason::Unknown), StopReason::Unknown);
    }

    #[test]
    fn stop_reason_display() {
        assert_eq!(StopReason::EndTurn.to_string(), "end_turn");
        assert_eq!(StopReason::ToolUse.to_string(), "tool_use");
    }

    #[test]
    fn event_data_serialization_roundtrip() {
        let data = EventData::TurnStart {
            turn_number: 1,
            thread_id: Uuid::nil(),
            model: "claude-4".into(),
            provider: "anthropic".into(),
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("\"kind\":\"turn_start\""));
        let _: EventData = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn event_data_tool_serialization() {
        let data = EventData::Tool {
            phase: ToolPhase::Progress {
                tool_call_id: "tc1".into(),
                name: "exec".into(),
                partial_result: "building...".into(),
            },
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("\"kind\":\"tool\""));
        assert!(json.contains("\"phase\":\"progress\""));
    }

    #[test]
    fn event_data_text_delta() {
        let data = EventData::TextDelta {
            delta: "Hello".into(),
            text: "Hello".into(),
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("\"kind\":\"text_delta\""));
    }

    #[tokio::test]
    async fn event_bus_emit_and_subscribe() {
        let bus = AgentEventBus::new(16);
        let mut rx = bus.subscribe();

        bus.emit(
            "run-1",
            EventStream::Lifecycle,
            EventData::TurnStart {
                turn_number: 0,
                thread_id: Uuid::nil(),
                model: "test".into(),
                provider: "test".into(),
            },
        );

        let event = rx.recv().await.unwrap();
        assert_eq!(event.run_id, "run-1");
        assert_eq!(event.seq, 1);
        assert_eq!(event.stream, EventStream::Lifecycle);
    }

    #[tokio::test]
    async fn event_bus_sequence_numbers_increment() {
        let bus = AgentEventBus::new(16);
        let mut rx = bus.subscribe();

        bus.emit(
            "run-1",
            EventStream::Assistant,
            EventData::TextDelta {
                delta: "a".into(),
                text: "a".into(),
            },
        );
        bus.emit(
            "run-1",
            EventStream::Assistant,
            EventData::TextDelta {
                delta: "b".into(),
                text: "ab".into(),
            },
        );

        let e1 = rx.recv().await.unwrap();
        let e2 = rx.recv().await.unwrap();
        assert_eq!(e1.seq, 1);
        assert_eq!(e2.seq, 2);
    }

    #[tokio::test]
    async fn event_bus_separate_runs_have_separate_seqs() {
        let bus = AgentEventBus::new(16);
        let mut rx = bus.subscribe();

        bus.emit(
            "run-1",
            EventStream::Lifecycle,
            EventData::Error {
                message: "test".into(),
                recoverable: None,
            },
        );
        bus.emit(
            "run-2",
            EventStream::Lifecycle,
            EventData::Error {
                message: "test".into(),
                recoverable: None,
            },
        );

        let e1 = rx.recv().await.unwrap();
        let e2 = rx.recv().await.unwrap();
        assert_eq!(e1.seq, 1);
        assert_eq!(e2.seq, 1); // separate run, separate counter
    }

    #[test]
    fn event_bus_finish_run_cleans_up() {
        let bus = AgentEventBus::new(16);
        bus.emit(
            "run-1",
            EventStream::Lifecycle,
            EventData::Error {
                message: "x".into(),
                recoverable: None,
            },
        );
        assert!(bus.seq_counters.contains_key("run-1"));
        bus.finish_run("run-1");
        assert!(!bus.seq_counters.contains_key("run-1"));
    }

    #[test]
    fn event_bus_subscriber_count() {
        let bus = AgentEventBus::new(16);
        assert_eq!(bus.subscriber_count(), 0);
        let _rx1 = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        let _rx2 = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);
    }

    #[test]
    fn agent_event_serialization() {
        let event = AgentEvent {
            run_id: "run-1".into(),
            seq: 1,
            ts: 1706547000000,
            stream: EventStream::Tool,
            data: EventData::Tool {
                phase: ToolPhase::Start {
                    tool_call_id: "tc1".into(),
                    name: "exec".into(),
                    arguments: serde_json::json!({}),
                },
            },
            session_key: Some("sess-1".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let restored: AgentEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.run_id, "run-1");
        assert_eq!(restored.seq, 1);
        assert_eq!(restored.stream, EventStream::Tool);
    }

    #[test]
    fn turn_result_complete() {
        let result = TurnResult {
            aborted: false,
            timed_out: false,
            assistant_texts: vec!["Hello!".into()],
            tool_calls: vec![],
            content_blocks: vec![ContentBlock::Text {
                text: "Hello!".into(),
            }],
            usage: TurnUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
                model: "claude-4".into(),
                provider: "anthropic".into(),
                duration_ms: 1234,
                ..Default::default()
            },
            stop_reason: StopReason::EndTurn,
            duration_ms: 1234,
        };
        assert!(!result.aborted);
        assert_eq!(result.assistant_texts.len(), 1);
        assert_eq!(result.usage.total_tokens, 150);
    }
}
