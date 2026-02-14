//! Team Executor — multi-agent collaboration with 5 execution modes.
//!
//! Modes:
//! - **Coordinator**: One agent routes tasks to specialists
//! - **RoundRobin**: Agents take turns processing messages
//! - **Delegate**: Picks the best agent for each task
//! - **Parallel**: All agents run simultaneously, results aggregated
//! - **Sequential**: Agents run in order, each receives prior output

use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::aria::agent_registry::AriaAgent;
use crate::aria::team_registry::AriaTeam;
use crate::aria::types::TeamMode;
use crate::llm::{ChatMessage, CompletionRequest, LlmProvider};
use crate::safety::SafetyLayer;

/// Result of a team execution.
#[derive(Debug, Clone)]
pub struct TeamResult {
    pub team_id: Uuid,
    pub mode: TeamMode,
    pub final_output: String,
    pub agent_outputs: Vec<AgentOutput>,
    pub total_turns: u32,
    pub duration: Duration,
}

/// Output from a single agent within a team.
#[derive(Debug, Clone)]
pub struct AgentOutput {
    pub agent_name: String,
    pub output: String,
    pub turn_number: u32,
}

/// Executes teams of agents according to the team's configured mode.
pub struct TeamExecutor {
    llm: Arc<dyn LlmProvider>,
    _safety: Arc<SafetyLayer>,
}

impl TeamExecutor {
    pub fn new(llm: Arc<dyn LlmProvider>, safety: Arc<SafetyLayer>) -> Self {
        Self {
            llm,
            _safety: safety,
        }
    }

    /// Execute a team on a given input message.
    pub async fn execute(
        &self,
        team: &AriaTeam,
        agents: &[AriaAgent],
        input: &str,
    ) -> Result<TeamResult, TeamError> {
        let mode = team.mode;

        let max_turns = team.max_turns.unwrap_or(10) as u32;
        let start = std::time::Instant::now();

        let result = match mode {
            TeamMode::Coordinator => {
                self.execute_coordinator(team, agents, input, max_turns)
                    .await?
            }
            TeamMode::RoundRobin => {
                self.execute_round_robin(team, agents, input, max_turns)
                    .await?
            }
            TeamMode::Delegate => self.execute_delegate(team, agents, input).await?,
            TeamMode::Parallel => self.execute_parallel(team, agents, input).await?,
            TeamMode::Sequential => self.execute_sequential(team, agents, input).await?,
        };

        Ok(TeamResult {
            team_id: team.id,
            mode,
            final_output: result.final_output,
            agent_outputs: result.agent_outputs,
            total_turns: result.total_turns,
            duration: start.elapsed(),
        })
    }

    /// Coordinator mode: one agent routes tasks to specialists.
    async fn execute_coordinator(
        &self,
        _team: &AriaTeam,
        agents: &[AriaAgent],
        input: &str,
        max_turns: u32,
    ) -> Result<IntermediateResult, TeamError> {
        let coordinator = agents.first().ok_or_else(|| {
            TeamError::InvalidConfig("coordinator mode requires at least one agent".into())
        })?;
        let specialists = &agents[1..];

        if specialists.is_empty() {
            // Single agent, just run it directly.
            let output = self.run_agent(coordinator, input).await?;
            return Ok(IntermediateResult {
                final_output: output.clone(),
                agent_outputs: vec![AgentOutput {
                    agent_name: coordinator.name.clone(),
                    output,
                    turn_number: 1,
                }],
                total_turns: 1,
            });
        }

        let specialist_list: String = specialists
            .iter()
            .map(|a| {
                let role = a.description.clone();
                format!("- {}: {}", a.name, role)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let coordinator_prompt = format!(
            "You are the coordinator of a team. Your job is to route the user's request \
             to the most appropriate specialist, then synthesize their responses.\n\n\
             Available specialists:\n{specialist_list}\n\n\
             Respond with the specialist name to delegate to, prefixed with DELEGATE: \
             or respond with FINAL: followed by your synthesized answer.\n\n\
             User request: {input}"
        );

        let mut all_outputs = Vec::new();
        let mut context = coordinator_prompt;
        let mut turn = 0;

        loop {
            turn += 1;
            if turn > max_turns {
                break;
            }

            let coordinator_response = self.run_agent(coordinator, &context).await?;
            all_outputs.push(AgentOutput {
                agent_name: coordinator.name.clone(),
                output: coordinator_response.clone(),
                turn_number: turn,
            });

            if let Some(final_answer) = coordinator_response.strip_prefix("FINAL:") {
                return Ok(IntermediateResult {
                    final_output: final_answer.trim().to_string(),
                    agent_outputs: all_outputs,
                    total_turns: turn,
                });
            }

            if let Some(delegate_to) = coordinator_response.strip_prefix("DELEGATE:") {
                let delegate_name = delegate_to.trim();
                if let Some(specialist) = specialists.iter().find(|a| a.name == delegate_name) {
                    turn += 1;
                    let specialist_output = self.run_agent(specialist, input).await?;
                    all_outputs.push(AgentOutput {
                        agent_name: specialist.name.clone(),
                        output: specialist_output.clone(),
                        turn_number: turn,
                    });
                    context = format!(
                        "Specialist '{}' responded:\n{}\n\n\
                         Based on this, provide your FINAL: answer or DELEGATE: to another specialist.",
                        specialist.name, specialist_output
                    );
                } else {
                    context = format!(
                        "Unknown specialist '{}'. Available: {}\n\
                         Try again with DELEGATE: or provide FINAL:",
                        delegate_name, specialist_list
                    );
                }
            } else {
                // If coordinator didn't use the protocol, treat as final.
                return Ok(IntermediateResult {
                    final_output: coordinator_response,
                    agent_outputs: all_outputs,
                    total_turns: turn,
                });
            }
        }

        // Max turns exceeded, use last output.
        let final_output = all_outputs
            .last()
            .map(|o| o.output.clone())
            .unwrap_or_default();
        Ok(IntermediateResult {
            final_output,
            agent_outputs: all_outputs,
            total_turns: turn,
        })
    }

    /// Round-robin mode: agents take turns.
    async fn execute_round_robin(
        &self,
        _team: &AriaTeam,
        agents: &[AriaAgent],
        input: &str,
        max_turns: u32,
    ) -> Result<IntermediateResult, TeamError> {
        if agents.is_empty() {
            return Err(TeamError::InvalidConfig("no agents in team".into()));
        }

        let mut all_outputs = Vec::new();
        let mut current_input = input.to_string();

        let turns = max_turns.min(agents.len() as u32);
        for (i, agent) in agents.iter().cycle().take(turns as usize).enumerate() {
            let output = self.run_agent(agent, &current_input).await?;
            all_outputs.push(AgentOutput {
                agent_name: agent.name.clone(),
                output: output.clone(),
                turn_number: (i + 1) as u32,
            });
            current_input = output;
        }

        let final_output = all_outputs
            .last()
            .map(|o| o.output.clone())
            .unwrap_or_default();
        Ok(IntermediateResult {
            final_output,
            agent_outputs: all_outputs,
            total_turns: turns,
        })
    }

    /// Delegate mode: pick the best agent for the task.
    async fn execute_delegate(
        &self,
        _team: &AriaTeam,
        agents: &[AriaAgent],
        input: &str,
    ) -> Result<IntermediateResult, TeamError> {
        if agents.is_empty() {
            return Err(TeamError::InvalidConfig("no agents in team".into()));
        }

        // Use the LLM to pick the best agent.
        let agent_descriptions: String = agents
            .iter()
            .map(|a| format!("- {}: {}", a.name, a.description))
            .collect::<Vec<_>>()
            .join("\n");

        let selection_prompt = format!(
            "Given this task, which agent is best suited to handle it? \
             Respond with ONLY the agent name.\n\n\
             Available agents:\n{agent_descriptions}\n\n\
             Task: {input}"
        );

        let selection = self.llm_complete(&selection_prompt).await?;
        let selected_name = selection.trim();

        let selected = agents
            .iter()
            .find(|a| a.name == selected_name)
            .unwrap_or(&agents[0]); // Fallback to first

        let output = self.run_agent(selected, input).await?;
        Ok(IntermediateResult {
            final_output: output.clone(),
            agent_outputs: vec![AgentOutput {
                agent_name: selected.name.clone(),
                output,
                turn_number: 1,
            }],
            total_turns: 1,
        })
    }

    /// Parallel mode: all agents run simultaneously.
    async fn execute_parallel(
        &self,
        _team: &AriaTeam,
        agents: &[AriaAgent],
        input: &str,
    ) -> Result<IntermediateResult, TeamError> {
        if agents.is_empty() {
            return Err(TeamError::InvalidConfig("no agents in team".into()));
        }

        let mut handles = Vec::new();
        for agent in agents {
            let llm = Arc::clone(&self.llm);
            let agent_clone = agent.clone();
            let input_clone = input.to_string();
            handles.push(tokio::spawn(async move {
                let messages = build_agent_messages(&agent_clone, &input_clone);
                let request = CompletionRequest::new(messages);
                let response = llm.complete(request).await;
                (agent_clone.name.clone(), response)
            }));
        }

        let mut all_outputs = Vec::new();
        let mut combined = Vec::new();
        for (i, handle) in handles.into_iter().enumerate() {
            let (name, result) = handle
                .await
                .map_err(|e| TeamError::ExecutionFailed(format!("join error: {e}")))?;
            let output = match result {
                Ok(r) => r.content,
                Err(e) => format!("[Error from {name}: {e}]"),
            };
            combined.push(format!("## {name}\n{output}"));
            all_outputs.push(AgentOutput {
                agent_name: name,
                output,
                turn_number: (i + 1) as u32,
            });
        }

        Ok(IntermediateResult {
            final_output: combined.join("\n\n"),
            agent_outputs: all_outputs,
            total_turns: agents.len() as u32,
        })
    }

    /// Sequential mode: agents run in order, each receives prior output.
    async fn execute_sequential(
        &self,
        _team: &AriaTeam,
        agents: &[AriaAgent],
        input: &str,
    ) -> Result<IntermediateResult, TeamError> {
        if agents.is_empty() {
            return Err(TeamError::InvalidConfig("no agents in team".into()));
        }

        let mut all_outputs = Vec::new();
        let mut current_input = input.to_string();

        for (i, agent) in agents.iter().enumerate() {
            let output = self.run_agent(agent, &current_input).await?;
            all_outputs.push(AgentOutput {
                agent_name: agent.name.clone(),
                output: output.clone(),
                turn_number: (i + 1) as u32,
            });
            current_input = output;
        }

        let final_output = all_outputs
            .last()
            .map(|o| o.output.clone())
            .unwrap_or_default();
        Ok(IntermediateResult {
            final_output,
            agent_outputs: all_outputs,
            total_turns: agents.len() as u32,
        })
    }

    /// Run a single agent on input.
    async fn run_agent(&self, agent: &AriaAgent, input: &str) -> Result<String, TeamError> {
        let messages = build_agent_messages(agent, input);
        let request = CompletionRequest::new(messages);
        let response =
            self.llm.complete(request).await.map_err(|e| {
                TeamError::ExecutionFailed(format!("agent '{}': {}", agent.name, e))
            })?;
        Ok(response.content)
    }

    /// Raw LLM completion for internal routing decisions.
    async fn llm_complete(&self, prompt: &str) -> Result<String, TeamError> {
        let messages = vec![ChatMessage::user(prompt)];
        let request = CompletionRequest::new(messages);
        let response = self
            .llm
            .complete(request)
            .await
            .map_err(|e| TeamError::ExecutionFailed(e.to_string()))?;
        Ok(response.content)
    }
}

/// Build the message list for an agent invocation.
fn build_agent_messages(agent: &AriaAgent, input: &str) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    if !agent.system_prompt.is_empty() {
        messages.push(ChatMessage::system(&agent.system_prompt));
    }
    messages.push(ChatMessage::user(input));
    messages
}

/// Internal intermediate result before wrapping in TeamResult.
struct IntermediateResult {
    final_output: String,
    agent_outputs: Vec<AgentOutput>,
    total_turns: u32,
}

/// Errors from team execution.
#[derive(Debug, thiserror::Error)]
pub enum TeamError {
    #[error("Invalid team configuration: {0}")]
    InvalidConfig(String),

    #[error("Execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Timeout after {0:?}")]
    Timeout(Duration),
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Utc;

    #[test]
    fn build_messages_with_system_prompt() {
        let agent = AriaAgent {
            id: Uuid::new_v4(),
            tenant_id: "test".into(),
            name: "test-agent".into(),
            description: "A test agent".into(),
            model: "gpt-4".into(),
            system_prompt: "You are helpful.".into(),
            tools: serde_json::json!([]),
            thinking_level: None,
            max_retries: 3,
            timeout_secs: 300,
            runtime_injections: serde_json::json!([]),
            status: "active".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let messages = build_agent_messages(&agent, "hello");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "You are helpful.");
        assert_eq!(messages[1].content, "hello");
    }

    #[test]
    fn build_messages_without_system_prompt() {
        let agent = AriaAgent {
            id: Uuid::new_v4(),
            tenant_id: "test".into(),
            name: "test-agent".into(),
            description: "A test agent".into(),
            model: "gpt-4".into(),
            system_prompt: String::new(),
            tools: serde_json::json!([]),
            thinking_level: None,
            max_retries: 3,
            timeout_secs: 300,
            runtime_injections: serde_json::json!([]),
            status: "active".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let messages = build_agent_messages(&agent, "hello");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "hello");
    }
}
