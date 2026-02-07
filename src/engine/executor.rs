use super::planner::ToolMapping;
use crate::connectors::registry::ConnectorRegistry;
use crate::error::KleviathanResult;

pub async fn execute_tool(
    mapping: &ToolMapping,
    registry: &ConnectorRegistry,
) -> KleviathanResult<serde_json::Value> {
    registry
        .execute(&mapping.tool, &mapping.action, mapping.parameters.clone())
        .await
}
