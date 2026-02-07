pub mod executor;
pub mod graph;
pub mod planner;
pub mod state;

use graph::TaskGraph;
use state::{ExecutionState, TaskState};

use crate::connectors::matrix::MatrixConnector;
use crate::connectors::slack;
use crate::error::KleviathanResult;
use crate::llm;
use crate::safety::{AbuseDetector, InjectionDetector, MessageRateLimiter};

enum ConversationPhase {
    Idle,
    AwaitingConfirmation {
        sender: String,
        original_prompt: String,
        task_graph: TaskGraph,
    },
}

#[tracing::instrument(skip_all, name = "engine::run")]
pub async fn run(
    config: crate::config::Config,
    disable_abuse_checks: bool,
) -> KleviathanResult<()> {
    let llm_provider = llm::create_provider(&config.llm)?;
    let enable_matrix_logs = config.matrix.enable_matrix_logs;
    let mut matrix = MatrixConnector::new(&config.matrix).await?;
    slack::verify_scopes(&config.slack).await?;
    slack::set_presence(&config.slack).await?;
    let rate_limiter = MessageRateLimiter::new();
    let registry = crate::connectors::build_registry(&config);
    let mut conversation_state = ConversationPhase::Idle;

    println!("KLEVIATHAN IS RUNNING. WAITING FOR MATRIX MESSAGES.");

    loop {
        let msg = match matrix.recv_message().await {
            Some(m) => m,
            None => break,
        };

        let phase_name = match &conversation_state {
            ConversationPhase::Idle => "idle",
            ConversationPhase::AwaitingConfirmation { .. } => "awaiting_confirmation",
        };
        if enable_matrix_logs {
            tracing::info!(
                sender = %msg.sender,
                message_text = %msg.text,
                conversation_phase = %phase_name,
                "Engine received Matrix message"
            );
        }

        if let Err(e) = rate_limiter.check() {
            tracing::warn!("Rate limited: {}", e);
            let _ = matrix
                .send_message(&msg.sender, "Rate limit exceeded. Please wait.")
                .await;
            continue;
        }

        match &conversation_state {
            ConversationPhase::Idle => {
                if !disable_abuse_checks {
                    if let Err(e) = AbuseDetector::check(&*llm_provider, &msg.text).await {
                        tracing::warn!("Abuse detected: {}", e);
                        let _ = matrix
                            .send_message(
                                &msg.sender,
                                "Message rejected: abusive content detected.",
                            )
                            .await;
                        continue;
                    }
                }

                if let Err(e) = InjectionDetector::check(&*llm_provider, &msg.text).await {
                    tracing::warn!("Injection detected: {}", e);
                    let _ = matrix
                        .send_message(
                            &msg.sender,
                            "Message rejected: potential code injection detected.",
                        )
                        .await;
                    continue;
                }

                let task_graph =
                    match planner::decompose_prompt(&*llm_provider, &msg.text, &registry).await {
                        Ok(g) => g,
                        Err(e) => {
                            tracing::error!("decompose_prompt failed: {}", e);
                            let _ = matrix
                                .send_message(&msg.sender, &format!("Failed to create plan: {}", e))
                                .await;
                            continue;
                        }
                    };

                let plan_text = planner::format_plan(&task_graph);
                let _ = matrix.send_message(&msg.sender, &plan_text).await;

                conversation_state = ConversationPhase::AwaitingConfirmation {
                    sender: msg.sender,
                    original_prompt: msg.text,
                    task_graph,
                };
            }

            ConversationPhase::AwaitingConfirmation {
                sender,
                original_prompt,
                task_graph,
            } => {
                if msg.sender != *sender {
                    continue;
                }

                let response = msg.text.trim().to_lowercase();

                if response == "yes" || response == "y" {
                    let task_graph = task_graph.clone();
                    let sender = sender.clone();
                    let original_prompt = original_prompt.clone();

                    let completion_msg =
                        execute_plan(&*llm_provider, &task_graph, &registry, &original_prompt)
                            .await;

                    let _ = matrix.send_message(&sender, &completion_msg).await;
                    conversation_state = ConversationPhase::Idle;
                } else if response == "no" || response == "n" {
                    let _ = matrix.send_message(sender, "Plan cancelled.").await;
                    conversation_state = ConversationPhase::Idle;
                } else {
                    let _ = matrix
                        .send_message(sender, "Please reply 'yes' to proceed or 'no' to cancel.")
                        .await;
                }
            }
        }
    }

    Ok(())
}

#[tracing::instrument(skip_all, fields(task_count = task_graph.tasks.len()))]
async fn execute_plan(
    llm_provider: &dyn crate::llm::LlmProvider,
    task_graph: &TaskGraph,
    registry: &crate::connectors::registry::ConnectorRegistry,
    original_prompt: &str,
) -> String {
    let ordered_tasks = match task_graph.topological_order() {
        Ok(t) => t.into_iter().cloned().collect::<Vec<_>>(),
        Err(e) => return format!("Plan error: {}", e),
    };

    let mut exec_state = ExecutionState::new(&ordered_tasks);
    let mut all_succeeded = true;

    for task in &ordered_tasks {
        exec_state.mark_running(&task.id);
        let dep_outputs = exec_state.get_dependency_outputs(task);

        let mapping =
            match planner::map_task_to_tool(llm_provider, task, &dep_outputs, registry).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!(task_id = %task.id, "map_task_to_tool failed: {}", e);
                    exec_state.mark_failed(&task.id, e.to_string());
                    all_succeeded = false;
                    break;
                }
            };

        match executor::execute_tool(&mapping, registry).await {
            Ok(output) => {
                exec_state.mark_completed(&task.id, output);
            }
            Err(e) => {
                tracing::error!(task_id = %task.id, "execute_tool failed: {}", e);
                exec_state.mark_failed(&task.id, e.to_string());
                all_succeeded = false;
                break;
            }
        }
    }

    if all_succeeded {
        summarize_results(llm_provider, original_prompt, &ordered_tasks, &exec_state)
            .await
            .unwrap_or_else(|e| {
                tracing::error!("Failed to summarize results: {}", e);
                "All tasks completed successfully.".to_string()
            })
    } else {
        let failed: Vec<String> = ordered_tasks
            .iter()
            .filter_map(|t| match exec_state.task_states.get(&t.id) {
                Some(TaskState::Failed { error }) => Some(format!("{}: {}", t.description, error)),
                _ => None,
            })
            .collect();
        let msg = format!("Execution finished with errors in: {}", failed.join(", "));
        tracing::error!("{}", msg);
        msg
    }
}

async fn summarize_results(
    llm_provider: &dyn crate::llm::LlmProvider,
    original_prompt: &str,
    tasks: &[graph::Task],
    exec_state: &ExecutionState,
) -> KleviathanResult<String> {
    let system_prompt = "You are a helpful assistant. Given the user's original question and the results from executing a series of tasks, provide a concise, direct answer to the user's question. Use only the data provided in the task results. Never return executable code.";

    let mut context = format!("User's question: {}\n\nTask results:\n", original_prompt);
    for task in tasks {
        if let Some(output) = exec_state.task_outputs.get(&task.id) {
            context.push_str(&format!(
                "- {} ({}): {}\n",
                task.description,
                task.id,
                serde_json::to_string(output).unwrap_or_default()
            ));
        }
    }

    let response = llm_provider.chat(system_prompt, &context, None).await?;
    Ok(response)
}
