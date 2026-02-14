//! FeedExecutor — runs feed handlers and produces feed items.
//!
//! Currently delegates to the LLM to produce feed content based on the
//! feed's handler code and context. When Quilt integration lands, this
//! will execute handler code inside sandboxed containers.

use std::sync::Arc;
use std::time::Duration;

use crate::aria::feed_registry::AriaFeed;
use crate::aria::types::{FeedItem, FeedResult};
use crate::llm::{ChatMessage, CompletionRequest, LlmProvider};

/// Executes feed handlers and produces typed feed items.
pub struct FeedExecutor {
    llm: Arc<dyn LlmProvider>,
}

impl FeedExecutor {
    pub fn new(llm: Arc<dyn LlmProvider>) -> Self {
        Self { llm }
    }

    /// Execute a feed and return its result.
    pub async fn execute(&self, feed: &AriaFeed) -> FeedResult {
        let prompt = format!(
            "You are a feed content generator. Generate content items for the '{}' feed.\n\n\
             Category: {}\n\
             Description: {}\n\n\
             Handler code (for context about what to generate):\n```\n{}\n```\n\n\
             Generate a JSON array of feed items. Each item should have:\n\
             - card_type: string (e.g., \"article\", \"news\", \"weather\")\n\
             - title: string\n\
             - body: string (optional)\n\
             - source: string (optional)\n\
             - url: string (optional)\n\n\
             Respond with ONLY the JSON array, no markdown wrapping.",
            feed.name, feed.category, feed.description, feed.handler_code
        );

        let messages = vec![ChatMessage::user(&prompt)];
        let request = CompletionRequest::new(messages).with_temperature(0.7);

        match tokio::time::timeout(Duration::from_secs(120), self.llm.complete(request)).await {
            Ok(Ok(response)) => {
                // Try to parse the response as a JSON array of items.
                match serde_json::from_str::<Vec<FeedItem>>(&response.content) {
                    Ok(items) => FeedResult {
                        success: true,
                        items,
                        summary: None,
                        metadata: None,
                        error: None,
                    },
                    Err(_) => {
                        // Try extracting JSON from markdown code blocks.
                        let cleaned = extract_json_array(&response.content);
                        match serde_json::from_str::<Vec<FeedItem>>(&cleaned) {
                            Ok(items) => FeedResult {
                                success: true,
                                items,
                                summary: None,
                                metadata: None,
                                error: None,
                            },
                            Err(e) => {
                                // Fall back to a single text item.
                                // Mark as degraded (success=false) so the scheduler
                                // knows this wasn't a clean run.
                                tracing::warn!(
                                    "Feed '{}' parse failed ({}), degraded to raw text",
                                    feed.name,
                                    e
                                );
                                FeedResult {
                                    success: false,
                                    items: vec![FeedItem {
                                        card_type: "article".into(),
                                        title: feed.name.clone(),
                                        body: Some(response.content),
                                        source: None,
                                        url: None,
                                        metadata: None,
                                        timestamp: None,
                                    }],
                                    summary: Some(format!(
                                        "Failed to parse structured items ({}), used raw text",
                                        e
                                    )),
                                    metadata: None,
                                    error: Some(format!("parse_degraded: {e}")),
                                }
                            }
                        }
                    }
                }
            }
            Ok(Err(e)) => FeedResult {
                success: false,
                items: vec![],
                summary: None,
                metadata: None,
                error: Some(format!("LLM error: {e}")),
            },
            Err(_) => FeedResult {
                success: false,
                items: vec![],
                summary: None,
                metadata: None,
                error: Some("Feed execution timed out".into()),
            },
        }
    }
}

/// Extract a JSON array from text that may be wrapped in markdown code blocks.
fn extract_json_array(text: &str) -> String {
    // Try to find ```json ... ``` blocks.
    if let Some(start) = text.find("```json") {
        let rest = &text[start + 7..];
        if let Some(end) = rest.find("```") {
            return rest[..end].trim().to_string();
        }
    }
    if let Some(start) = text.find("```") {
        let rest = &text[start + 3..];
        if let Some(end) = rest.find("```") {
            return rest[..end].trim().to_string();
        }
    }
    // Try to find raw [ ... ] in the text.
    if let Some(start) = text.find('[') {
        if let Some(end) = text.rfind(']') {
            return text[start..=end].to_string();
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_markdown() {
        let text =
            "Here are the items:\n```json\n[{\"card_type\":\"news\",\"title\":\"Hello\"}]\n```\n";
        let result = extract_json_array(text);
        assert!(result.starts_with('['));
        let items: Vec<FeedItem> = serde_json::from_str(&result).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Hello");
    }

    #[test]
    fn extract_json_raw_array() {
        let text = "Some text [{\"card_type\":\"article\",\"title\":\"Test\"}] more text";
        let result = extract_json_array(text);
        let items: Vec<FeedItem> = serde_json::from_str(&result).unwrap();
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn feed_item_deserialize() {
        let json = r#"{"card_type":"weather","title":"Sunny Day","body":"72°F"}"#;
        let item: FeedItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.card_type, "weather");
        assert_eq!(item.title, "Sunny Day");
        assert_eq!(item.body, Some("72°F".into()));
        assert!(item.url.is_none());
    }
}
