use std::collections::HashMap;

use super::graph::Task;

#[derive(Debug, Clone)]
pub enum TaskState {
    Pending,
    Running,
    Completed,
    Failed { error: String },
}

#[derive(Debug)]
pub struct ExecutionState {
    pub task_states: HashMap<String, TaskState>,
    pub task_outputs: HashMap<String, serde_json::Value>,
}

impl ExecutionState {
    pub fn new(tasks: &[Task]) -> Self {
        let task_states = tasks
            .iter()
            .map(|t| (t.id.clone(), TaskState::Pending))
            .collect();
        Self {
            task_states,
            task_outputs: HashMap::new(),
        }
    }

    pub fn mark_running(&mut self, task_id: &str) {
        self.task_states
            .insert(task_id.to_string(), TaskState::Running);
    }

    pub fn mark_completed(&mut self, task_id: &str, output: serde_json::Value) {
        self.task_states
            .insert(task_id.to_string(), TaskState::Completed);
        self.task_outputs.insert(task_id.to_string(), output);
    }

    pub fn mark_failed(&mut self, task_id: &str, error: String) {
        self.task_states
            .insert(task_id.to_string(), TaskState::Failed { error });
    }

    pub fn get_dependency_outputs(&self, task: &Task) -> HashMap<String, serde_json::Value> {
        task.depends_on
            .iter()
            .filter_map(|dep_id| {
                self.task_outputs
                    .get(dep_id)
                    .map(|output| (dep_id.clone(), output.clone()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tasks() -> Vec<Task> {
        vec![
            Task {
                id: "a".into(),
                description: "Task A".into(),
                depends_on: vec![],
            },
            Task {
                id: "b".into(),
                description: "Task B".into(),
                depends_on: vec!["a".into()],
            },
            Task {
                id: "c".into(),
                description: "Task C".into(),
                depends_on: vec!["a".into(), "b".into()],
            },
        ]
    }

    #[test]
    fn new_state_all_pending() {
        let tasks = make_tasks();
        let state = ExecutionState::new(&tasks);

        for task in &tasks {
            assert!(matches!(
                state.task_states.get(&task.id),
                Some(TaskState::Pending)
            ));
        }
    }

    #[test]
    fn mark_running_transitions_state() {
        let tasks = make_tasks();
        let mut state = ExecutionState::new(&tasks);

        state.mark_running("a");
        assert!(matches!(
            state.task_states.get("a"),
            Some(TaskState::Running)
        ));
    }

    #[test]
    fn mark_completed_stores_output() {
        let tasks = make_tasks();
        let mut state = ExecutionState::new(&tasks);

        let output = serde_json::json!({"result": "done"});
        state.mark_completed("a", output.clone());

        assert!(matches!(
            state.task_states.get("a"),
            Some(TaskState::Completed)
        ));
        assert_eq!(state.task_outputs.get("a"), Some(&output));
    }

    #[test]
    fn mark_failed_stores_error() {
        let tasks = make_tasks();
        let mut state = ExecutionState::new(&tasks);

        state.mark_failed("a", "something broke".into());
        match state.task_states.get("a") {
            Some(TaskState::Failed { error }) => {
                assert_eq!(error, "something broke");
            }
            _ => panic!("Expected Failed state"),
        }
    }

    #[test]
    fn get_dependency_outputs_returns_completed_outputs() {
        let tasks = make_tasks();
        let mut state = ExecutionState::new(&tasks);

        let output_a = serde_json::json!({"data": "from_a"});
        let output_b = serde_json::json!({"data": "from_b"});
        state.mark_completed("a", output_a.clone());
        state.mark_completed("b", output_b.clone());

        let deps = state.get_dependency_outputs(&tasks[2]);
        assert_eq!(deps.len(), 2);
        assert_eq!(deps.get("a"), Some(&output_a));
        assert_eq!(deps.get("b"), Some(&output_b));
    }

    #[test]
    fn get_dependency_outputs_skips_incomplete() {
        let tasks = make_tasks();
        let mut state = ExecutionState::new(&tasks);

        state.mark_completed("a", serde_json::json!("ok"));

        let deps = state.get_dependency_outputs(&tasks[2]);
        assert_eq!(deps.len(), 1);
        assert!(deps.contains_key("a"));
        assert!(!deps.contains_key("b"));
    }
}
