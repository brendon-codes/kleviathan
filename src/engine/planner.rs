use std::collections::HashMap;

use serde::Deserialize;

use super::graph::{Task, TaskGraph};
use crate::connectors::registry::ConnectorRegistry;
use crate::error::KleviathanResult;
use crate::llm::LlmProvider;

#[derive(Debug, Deserialize)]
pub struct ToolMapping {
    pub tool: String,
    pub action: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ActionSelection {
    tool: String,
    action: String,
}

#[tracing::instrument(skip(llm, registry), fields(prompt_len = prompt.len()))]
pub async fn decompose_prompt(
    llm: &dyn LlmProvider,
    prompt: &str,
    registry: &ConnectorRegistry,
) -> KleviathanResult<TaskGraph> {
    let tool_descriptions = registry.tool_action_descriptions();
    let system_prompt = format!(
        "You are a task planning assistant. Given a user request, decompose it into a directed acyclic graph of discrete tasks. Each task should be a single actionable step. Tasks may depend on the outputs of other tasks. Return a JSON object with a 'tasks' array. Each task has an 'id' (short string), 'description' (what to do), and 'depends_on' (array of task IDs this task needs completed first). Never return executable code.\n\nAvailable tool actions:\n{}\n\nIMPORTANT: When a task requires a resource ID (such as calendar_id or addressbook_id), include a preceding discovery task (e.g., list_calendars before search_events, list_addressbooks before search_contacts). Each task should map to one available tool action.",
        tool_descriptions
    );

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "description": { "type": "string" },
                        "depends_on": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    },
                    "required": ["id", "description", "depends_on"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["tasks"],
        "additionalProperties": false
    });

    let response = llm.chat(&system_prompt, prompt, Some(&schema)).await?;
    let task_graph: TaskGraph = serde_json::from_str(&response)?;
    task_graph.validate()?;
    Ok(task_graph)
}

pub fn format_plan(task_graph: &TaskGraph) -> String {
    let mut plan = String::from("Here's my plan:\n\n");
    for (i, task) in task_graph.tasks.iter().enumerate() {
        plan.push_str(&format!("{}. {}\n", i + 1, task.description));
        if !task.depends_on.is_empty() {
            plan.push_str(&format!("   (depends on: {})\n", task.depends_on.join(", ")));
        }
    }
    plan.push_str("\nReply 'yes' to proceed or 'no' to cancel.");
    plan
}

async fn select_action(
    llm: &dyn LlmProvider,
    task: &Task,
    dependency_outputs: &HashMap<String, serde_json::Value>,
    registry: &ConnectorRegistry,
) -> KleviathanResult<ActionSelection> {
    let system_prompt = format!(
        "You are a tool mapping assistant. Given a task description, select the correct tool and action. Available tools: {}. Tool-action mapping: {}. Return the tool and action as JSON. Never return executable code.",
        registry.available_tools().join(", "),
        registry.tool_action_descriptions()
    );

    let context = if dependency_outputs.is_empty() {
        task.description.clone()
    } else {
        format!(
            "Task: {}\n\nContext from previous tasks:\n{}",
            task.description,
            serde_json::to_string_pretty(dependency_outputs).unwrap_or_default()
        )
    };

    let schema = registry.action_selection_schema();
    let response = llm.chat(&system_prompt, &context, Some(&schema)).await?;
    let selection: ActionSelection = serde_json::from_str(&response)?;
    Ok(selection)
}

async fn extract_parameters(
    llm: &dyn LlmProvider,
    task: &Task,
    dependency_outputs: &HashMap<String, serde_json::Value>,
    tool: &str,
    action: &str,
    registry: &ConnectorRegistry,
) -> KleviathanResult<serde_json::Value> {
    let constraint_note = registry.constraint_note_for(tool, action);
    let system_prompt = format!(
        "You are a parameter extraction assistant. Extract the parameters for the {}/{} action from the task description.{} Never return executable code.",
        tool, action, constraint_note
    );

    let context = if dependency_outputs.is_empty() {
        task.description.clone()
    } else {
        format!(
            "Task: {}\n\nContext from previous tasks:\n{}",
            task.description,
            serde_json::to_string_pretty(dependency_outputs).unwrap_or_default()
        )
    };

    let schema = registry.parameter_schema_for(tool, action)?;
    let response = llm.chat(&system_prompt, &context, Some(&schema)).await?;
    let parameters: serde_json::Value = serde_json::from_str(&response)?;
    Ok(parameters)
}

#[tracing::instrument(skip(llm, dependency_outputs, registry), fields(task_id = %task.id))]
pub async fn map_task_to_tool(
    llm: &dyn LlmProvider,
    task: &Task,
    dependency_outputs: &HashMap<String, serde_json::Value>,
    registry: &ConnectorRegistry,
) -> KleviathanResult<ToolMapping> {
    let selection = select_action(llm, task, dependency_outputs, registry).await?;
    let _schema = registry.parameter_schema_for(&selection.tool, &selection.action)?;
    let parameters = extract_parameters(
        llm,
        task,
        dependency_outputs,
        &selection.tool,
        &selection.action,
        registry,
    )
    .await?;
    Ok(ToolMapping {
        tool: selection.tool,
        action: selection.action,
        parameters,
    })
}
