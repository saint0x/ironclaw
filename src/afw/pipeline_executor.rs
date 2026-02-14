//! Pipeline Executor — DAG-based workflow execution.
//!
//! Pipelines chain agents, tools, and teams into multi-step workflows
//! with dependency graphs, conditional execution, and retry policies.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::aria::pipeline_registry::{AriaPipeline, PipelineRegistry};
use crate::aria::types::{PipelineStep, RetryPolicy};
use crate::llm::{ChatMessage, CompletionRequest, LlmProvider};
use crate::safety::SafetyLayer;

/// Result of a pipeline execution.
#[derive(Debug, Clone)]
pub struct PipelineResult {
    pub pipeline_id: Uuid,
    pub run_id: Uuid,
    pub status: PipelineStatus,
    pub step_results: Vec<StepResult>,
    pub final_output: Option<String>,
    pub duration: Duration,
}

/// Status of a pipeline run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineStatus {
    Completed,
    Failed,
    PartialSuccess,
}

/// Result of a single pipeline step.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StepResult {
    pub step_name: String,
    pub status: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub retries_used: u32,
}

/// Executes pipelines as DAGs of steps.
pub struct PipelineExecutor {
    llm: Arc<dyn LlmProvider>,
    _safety: Arc<SafetyLayer>,
    pipeline_registry: Arc<PipelineRegistry>,
}

impl PipelineExecutor {
    pub fn new(
        llm: Arc<dyn LlmProvider>,
        safety: Arc<SafetyLayer>,
        pipeline_registry: Arc<PipelineRegistry>,
    ) -> Self {
        Self {
            llm,
            _safety: safety,
            pipeline_registry,
        }
    }

    /// Execute a pipeline with the given input variables.
    pub async fn execute(
        &self,
        pipeline: &AriaPipeline,
        input_variables: serde_json::Value,
    ) -> Result<PipelineResult, PipelineError> {
        let start = std::time::Instant::now();

        // Parse steps from the pipeline definition.
        let steps: Vec<PipelineStep> = serde_json::from_value(pipeline.steps.clone())
            .map_err(|e| PipelineError::InvalidConfig(format!("bad steps JSON: {e}")))?;

        if steps.is_empty() {
            return Err(PipelineError::InvalidConfig("pipeline has no steps".into()));
        }

        // Validate the DAG (no cycles).
        self.validate_dag(&steps)?;

        // Create pipeline run record.
        let run = self
            .pipeline_registry
            .create_run(
                pipeline.id,
                &pipeline.tenant_id,
                Some(input_variables.clone()),
            )
            .await
            .map_err(|e| PipelineError::RegistryError(e.to_string()))?;
        let run_id = run.id;

        // Execute steps in topological order.
        let mut completed: HashMap<String, StepResult> = HashMap::new();
        let mut variables = input_variables;
        let mut all_results = Vec::new();

        // Build dependency graph.
        let execution_order = self.topological_sort(&steps)?;

        for step_name in &execution_order {
            let step = steps.iter().find(|s| &s.name == step_name).ok_or_else(|| {
                PipelineError::InvalidConfig(format!("step '{step_name}' not found"))
            })?;

            // Check if all dependencies are met.
            for dep in &step.depends_on {
                if !completed.contains_key(dep) {
                    return Err(PipelineError::DependencyFailed {
                        step: step_name.clone(),
                        dependency: dep.clone(),
                    });
                }
                // Check dependency didn't fail.
                if let Some(dep_result) = completed.get(dep) {
                    if dep_result.status == "failed" {
                        let result = StepResult {
                            step_name: step_name.clone(),
                            status: "skipped".into(),
                            output: None,
                            error: Some(format!("dependency '{dep}' failed")),
                            duration_ms: 0,
                            retries_used: 0,
                        };
                        completed.insert(step_name.clone(), result.clone());
                        all_results.push(result);
                        continue;
                    }
                }
            }

            // Evaluate condition if present.
            if let Some(ref condition) = step.condition {
                if !self.evaluate_condition(condition, &completed, &variables) {
                    let result = StepResult {
                        step_name: step_name.clone(),
                        status: "skipped".into(),
                        output: None,
                        error: None,
                        duration_ms: 0,
                        retries_used: 0,
                    };
                    completed.insert(step_name.clone(), result.clone());
                    all_results.push(result);
                    continue;
                }
            }

            // Execute the step with retry.
            let result = self.execute_step(step, &completed, &variables).await;

            let step_result = match result {
                Ok(r) => r,
                Err(e) => StepResult {
                    step_name: step_name.clone(),
                    status: "failed".into(),
                    output: None,
                    error: Some(e.to_string()),
                    duration_ms: 0,
                    retries_used: 0,
                },
            };

            // Update variables with step output.
            if let Some(ref output) = step_result.output {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(output) {
                    if let serde_json::Value::Object(ref map) = variables {
                        let mut new_vars = map.clone();
                        new_vars.insert(step_name.clone(), val);
                        variables = serde_json::Value::Object(new_vars);
                    }
                }
            }

            completed.insert(step_name.clone(), step_result.clone());
            all_results.push(step_result);

            // Update run progress.
            let _ = self
                .pipeline_registry
                .update_run(
                    run_id,
                    "running",
                    Some(step_name.as_str()),
                    Some(serde_json::to_value(&all_results).unwrap_or_default()),
                    None,
                )
                .await;
        }

        // Determine final status.
        let has_failures = all_results.iter().any(|r| r.status == "failed");
        let has_successes = all_results.iter().any(|r| r.status == "completed");
        let status = if has_failures && has_successes {
            PipelineStatus::PartialSuccess
        } else if has_failures {
            PipelineStatus::Failed
        } else {
            PipelineStatus::Completed
        };

        let final_output = all_results.last().and_then(|r| r.output.clone());

        // Update final run status.
        let status_str = match status {
            PipelineStatus::Completed => "completed",
            PipelineStatus::Failed => "failed",
            PipelineStatus::PartialSuccess => "partial",
        };
        let _ = self
            .pipeline_registry
            .update_run(
                run_id,
                status_str,
                None,
                Some(serde_json::to_value(&all_results).unwrap_or_default()),
                if has_failures {
                    Some("one or more steps failed")
                } else {
                    None
                },
            )
            .await;

        Ok(PipelineResult {
            pipeline_id: pipeline.id,
            run_id,
            status,
            step_results: all_results,
            final_output,
            duration: start.elapsed(),
        })
    }

    /// Execute a single step with retry policy.
    async fn execute_step(
        &self,
        step: &PipelineStep,
        completed: &HashMap<String, StepResult>,
        variables: &serde_json::Value,
    ) -> Result<StepResult, PipelineError> {
        let retry = step.retry.clone().unwrap_or(RetryPolicy {
            max_retries: 0,
            backoff_ms: 1000,
            backoff_multiplier: 2.0,
        });

        let mut last_error = None;
        let mut retries_used = 0;

        for attempt in 0..=retry.max_retries {
            if attempt > 0 {
                let delay =
                    retry.backoff_ms as f64 * retry.backoff_multiplier.powi(attempt as i32 - 1);
                tokio::time::sleep(Duration::from_millis(delay as u64)).await;
                retries_used = attempt;
            }

            let step_start = std::time::Instant::now();

            match self.run_step_inner(step, completed, variables).await {
                Ok(output) => {
                    return Ok(StepResult {
                        step_name: step.name.clone(),
                        status: "completed".into(),
                        output: Some(output),
                        error: None,
                        duration_ms: step_start.elapsed().as_millis() as u64,
                        retries_used,
                    });
                }
                Err(e) => {
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| PipelineError::StepFailed {
            step: step.name.clone(),
            reason: "unknown error".into(),
        }))
    }

    /// Inner step execution — determines what to run based on the `execute` field.
    async fn run_step_inner(
        &self,
        step: &PipelineStep,
        _completed: &HashMap<String, StepResult>,
        variables: &serde_json::Value,
    ) -> Result<String, PipelineError> {
        // Build the prompt from step params + variables context.
        let params_str = serde_json::to_string_pretty(&step.params).unwrap_or_default();
        let vars_str = serde_json::to_string_pretty(variables).unwrap_or_default();

        let prompt = format!(
            "Execute step '{}' of the pipeline.\n\n\
             Step type: {}\n\
             Parameters: {}\n\
             Available variables: {}",
            step.name, step.execute, params_str, vars_str
        );

        let messages = vec![ChatMessage::user(&prompt)];
        let request = CompletionRequest::new(messages);

        let timeout = Duration::from_secs(step.timeout_secs.unwrap_or(300));
        let response = tokio::time::timeout(timeout, self.llm.complete(request))
            .await
            .map_err(|_| PipelineError::StepFailed {
                step: step.name.clone(),
                reason: format!("timeout after {timeout:?}"),
            })?
            .map_err(|e| PipelineError::StepFailed {
                step: step.name.clone(),
                reason: e.to_string(),
            })?;

        Ok(response.content)
    }

    /// Validate that the step graph is a DAG (no cycles).
    fn validate_dag(&self, steps: &[PipelineStep]) -> Result<(), PipelineError> {
        let names: HashSet<&str> = steps.iter().map(|s| s.name.as_str()).collect();

        for step in steps {
            for dep in &step.depends_on {
                if !names.contains(dep.as_str()) {
                    return Err(PipelineError::InvalidConfig(format!(
                        "step '{}' depends on unknown step '{}'",
                        step.name, dep
                    )));
                }
            }
        }

        // Cycle detection via topological sort.
        self.topological_sort(steps)?;
        Ok(())
    }

    /// Topological sort of steps (Kahn's algorithm).
    fn topological_sort(&self, steps: &[PipelineStep]) -> Result<Vec<String>, PipelineError> {
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

        for step in steps {
            in_degree.entry(step.name.as_str()).or_insert(0);
            for dep in &step.depends_on {
                *in_degree.entry(step.name.as_str()).or_insert(0) += 1;
                dependents
                    .entry(dep.as_str())
                    .or_default()
                    .push(step.name.as_str());
            }
        }

        let mut queue: Vec<&str> = in_degree
            .iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(name, _)| *name)
            .collect();
        queue.sort(); // Deterministic order

        let mut result = Vec::new();

        while let Some(name) = queue.pop() {
            result.push(name.to_string());
            if let Some(deps) = dependents.get(name) {
                for &dep in deps {
                    if let Some(deg) = in_degree.get_mut(dep) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push(dep);
                        }
                    }
                }
            }
        }

        if result.len() != steps.len() {
            return Err(PipelineError::InvalidConfig(
                "cycle detected in pipeline steps".into(),
            ));
        }

        Ok(result)
    }

    /// Evaluate a simple condition expression.
    fn evaluate_condition(
        &self,
        condition: &str,
        completed: &HashMap<String, StepResult>,
        _variables: &serde_json::Value,
    ) -> bool {
        // Simple condition syntax: "step_name.status == completed"
        let parts: Vec<&str> = condition.split_whitespace().collect();
        if parts.len() >= 3 {
            let field = parts[0];
            let op = parts[1];
            let value = parts[2];

            if let Some(dot_pos) = field.find('.') {
                let step_name = &field[..dot_pos];
                let prop = &field[dot_pos + 1..];

                if let Some(result) = completed.get(step_name) {
                    let actual = match prop {
                        "status" => &result.status,
                        _ => return false,
                    };
                    return match op {
                        "==" => actual == value,
                        "!=" => actual != value,
                        _ => false,
                    };
                }
            }
        }

        // Default: condition not understood → run the step.
        true
    }
}

/// Errors from pipeline execution.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("Invalid pipeline configuration: {0}")]
    InvalidConfig(String),

    #[error("Step '{step}' failed: {reason}")]
    StepFailed { step: String, reason: String },

    #[error("Dependency '{dependency}' failed for step '{step}'")]
    DependencyFailed { step: String, dependency: String },

    #[error("Registry error: {0}")]
    RegistryError(String),

    #[error("Timeout after {0:?}")]
    Timeout(Duration),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_step(name: &str, deps: &[&str]) -> PipelineStep {
        PipelineStep {
            name: name.into(),
            execute: "agent:test".into(),
            params: serde_json::json!({}),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            condition: None,
            retry: None,
            timeout_secs: None,
        }
    }

    #[test]
    fn topological_sort_linear() {
        let steps = vec![
            make_step("a", &[]),
            make_step("b", &["a"]),
            make_step("c", &["b"]),
        ];
        let executor = PipelineExecutor::new(
            Arc::new(crate::llm::tests::MockLlmProvider),
            Arc::new(SafetyLayer::default()),
            Arc::new(PipelineRegistry::new(
                deadpool_postgres::Pool::builder(deadpool_postgres::Manager::new(
                    "host=localhost".parse().unwrap(),
                    tokio_postgres::NoTls,
                ))
                .build()
                .unwrap(),
            )),
        );
        let order = executor.topological_sort(&steps).unwrap();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn topological_sort_diamond() {
        let steps = vec![
            make_step("a", &[]),
            make_step("b", &["a"]),
            make_step("c", &["a"]),
            make_step("d", &["b", "c"]),
        ];
        let executor = PipelineExecutor::new(
            Arc::new(crate::llm::tests::MockLlmProvider),
            Arc::new(SafetyLayer::default()),
            Arc::new(PipelineRegistry::new(
                deadpool_postgres::Pool::builder(deadpool_postgres::Manager::new(
                    "host=localhost".parse().unwrap(),
                    tokio_postgres::NoTls,
                ))
                .build()
                .unwrap(),
            )),
        );
        let order = executor.topological_sort(&steps).unwrap();
        // "a" must come before "b" and "c", "d" must be last.
        assert_eq!(order[0], "a");
        assert_eq!(order[3], "d");
    }

    #[test]
    fn topological_sort_cycle_detected() {
        let steps = vec![
            make_step("a", &["c"]),
            make_step("b", &["a"]),
            make_step("c", &["b"]),
        ];
        let executor = PipelineExecutor::new(
            Arc::new(crate::llm::tests::MockLlmProvider),
            Arc::new(SafetyLayer::default()),
            Arc::new(PipelineRegistry::new(
                deadpool_postgres::Pool::builder(deadpool_postgres::Manager::new(
                    "host=localhost".parse().unwrap(),
                    tokio_postgres::NoTls,
                ))
                .build()
                .unwrap(),
            )),
        );
        let result = executor.topological_sort(&steps);
        assert!(result.is_err());
    }

    #[test]
    fn condition_evaluation() {
        let executor = PipelineExecutor::new(
            Arc::new(crate::llm::tests::MockLlmProvider),
            Arc::new(SafetyLayer::default()),
            Arc::new(PipelineRegistry::new(
                deadpool_postgres::Pool::builder(deadpool_postgres::Manager::new(
                    "host=localhost".parse().unwrap(),
                    tokio_postgres::NoTls,
                ))
                .build()
                .unwrap(),
            )),
        );
        let mut completed = HashMap::new();
        completed.insert(
            "step_a".to_string(),
            StepResult {
                step_name: "step_a".into(),
                status: "completed".into(),
                output: None,
                error: None,
                duration_ms: 0,
                retries_used: 0,
            },
        );

        assert!(executor.evaluate_condition(
            "step_a.status == completed",
            &completed,
            &serde_json::json!({})
        ));
        assert!(!executor.evaluate_condition(
            "step_a.status == failed",
            &completed,
            &serde_json::json!({})
        ));
    }
}
