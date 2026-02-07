use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::error::{KleviathanError, KleviathanResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGraph {
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub depends_on: Vec<String>,
}

impl TaskGraph {
    pub fn topological_order(&self) -> KleviathanResult<Vec<&Task>> {
        let task_map: HashMap<&str, &Task> =
            self.tasks.iter().map(|t| (t.id.as_str(), t)).collect();

        let mut in_degree: HashMap<&str, usize> =
            self.tasks.iter().map(|t| (t.id.as_str(), 0)).collect();

        for task in &self.tasks {
            for dep in &task.depends_on {
                *in_degree.entry(task.id.as_str()).or_default() += 1;
                let _ = in_degree.entry(dep.as_str()).or_default();
            }
        }

        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|&(_, deg)| *deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut result: Vec<&Task> = Vec::with_capacity(self.tasks.len());

        while let Some(current) = queue.pop_front() {
            if let Some(&task) = task_map.get(current) {
                result.push(task);
            }

            for task in &self.tasks {
                if task.depends_on.iter().any(|d| d == current) {
                    let deg = in_degree.get_mut(task.id.as_str()).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(task.id.as_str());
                    }
                }
            }
        }

        if result.len() != self.tasks.len() {
            return Err(KleviathanError::TaskGraph(
                "Cycle detected in task dependency graph".into(),
            ));
        }

        Ok(result)
    }

    pub fn validate(&self) -> KleviathanResult<()> {
        if self.tasks.is_empty() {
            return Err(KleviathanError::TaskGraph(
                "Task graph must contain at least one task".into(),
            ));
        }

        let mut seen_ids = HashSet::new();
        for task in &self.tasks {
            if !seen_ids.insert(&task.id) {
                return Err(KleviathanError::TaskGraph(format!(
                    "Duplicate task ID: {}",
                    task.id
                )));
            }
        }

        for task in &self.tasks {
            for dep in &task.depends_on {
                if !seen_ids.contains(dep) {
                    return Err(KleviathanError::TaskGraph(format!(
                        "Task '{}' depends on unknown task '{}'",
                        task.id, dep
                    )));
                }
            }
        }

        self.topological_order()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topological_order_simple_dag() {
        let graph = TaskGraph {
            tasks: vec![
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
                    depends_on: vec!["a".into()],
                },
                Task {
                    id: "d".into(),
                    description: "Task D".into(),
                    depends_on: vec!["b".into(), "c".into()],
                },
            ],
        };

        let order = graph.topological_order().unwrap();
        let ids: Vec<&str> = order.iter().map(|t| t.id.as_str()).collect();

        let pos = |id: &str| ids.iter().position(|&x| x == id).unwrap();
        assert!(pos("a") < pos("b"));
        assert!(pos("a") < pos("c"));
        assert!(pos("b") < pos("d"));
        assert!(pos("c") < pos("d"));
    }

    #[test]
    fn topological_order_single_task() {
        let graph = TaskGraph {
            tasks: vec![Task {
                id: "only".into(),
                description: "Only task".into(),
                depends_on: vec![],
            }],
        };

        let order = graph.topological_order().unwrap();
        assert_eq!(order.len(), 1);
        assert_eq!(order[0].id, "only");
    }

    #[test]
    fn topological_order_detects_cycle() {
        let graph = TaskGraph {
            tasks: vec![
                Task {
                    id: "a".into(),
                    description: "Task A".into(),
                    depends_on: vec!["b".into()],
                },
                Task {
                    id: "b".into(),
                    description: "Task B".into(),
                    depends_on: vec!["a".into()],
                },
            ],
        };

        let result = graph.topological_order();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Cycle"));
    }

    #[test]
    fn validate_rejects_empty_graph() {
        let graph = TaskGraph { tasks: vec![] };
        let result = graph.validate();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("at least one task"));
    }

    #[test]
    fn validate_rejects_duplicate_ids() {
        let graph = TaskGraph {
            tasks: vec![
                Task {
                    id: "a".into(),
                    description: "First".into(),
                    depends_on: vec![],
                },
                Task {
                    id: "a".into(),
                    description: "Duplicate".into(),
                    depends_on: vec![],
                },
            ],
        };

        let result = graph.validate();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Duplicate"));
    }

    #[test]
    fn validate_rejects_unknown_dependency() {
        let graph = TaskGraph {
            tasks: vec![Task {
                id: "a".into(),
                description: "Task A".into(),
                depends_on: vec!["nonexistent".into()],
            }],
        };

        let result = graph.validate();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown task"));
    }

    #[test]
    fn validate_accepts_valid_graph() {
        let graph = TaskGraph {
            tasks: vec![
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
            ],
        };

        assert!(graph.validate().is_ok());
    }

    #[test]
    fn topological_order_linear_chain() {
        let graph = TaskGraph {
            tasks: vec![
                Task {
                    id: "1".into(),
                    description: "First".into(),
                    depends_on: vec![],
                },
                Task {
                    id: "2".into(),
                    description: "Second".into(),
                    depends_on: vec!["1".into()],
                },
                Task {
                    id: "3".into(),
                    description: "Third".into(),
                    depends_on: vec!["2".into()],
                },
            ],
        };

        let order = graph.topological_order().unwrap();
        let ids: Vec<&str> = order.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["1", "2", "3"]);
    }

    #[test]
    fn topological_order_independent_tasks() {
        let graph = TaskGraph {
            tasks: vec![
                Task {
                    id: "a".into(),
                    description: "A".into(),
                    depends_on: vec![],
                },
                Task {
                    id: "b".into(),
                    description: "B".into(),
                    depends_on: vec![],
                },
                Task {
                    id: "c".into(),
                    description: "C".into(),
                    depends_on: vec![],
                },
            ],
        };

        let order = graph.topological_order().unwrap();
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn topological_order_self_cycle() {
        let graph = TaskGraph {
            tasks: vec![Task {
                id: "a".into(),
                description: "Self-referencing".into(),
                depends_on: vec!["a".into()],
            }],
        };

        assert!(graph.topological_order().is_err());
    }
}
