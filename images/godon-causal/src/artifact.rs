use crate::graph::CausalGraph;

pub fn export_artifact(graph: &CausalGraph) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(graph)
}

pub fn import_artifact(json: &str) -> Result<CausalGraph, serde_json::Error> {
    serde_json::from_str(json)
}
