use crate::graph::{CausalGraph, GraphSummary, Prediction};

pub struct QueryEngine<'a> {
    graph: &'a CausalGraph,
}

impl<'a> QueryEngine<'a> {
    pub fn new(graph: &'a CausalGraph) -> Self {
        Self { graph }
    }

    /// "If sender probes at scale S, what happens to everyone?"
    pub fn what_if(&self, sender_id: &str, impulse_scale: f64) -> Vec<Prediction> {
        self.graph.predict(sender_id, impulse_scale)
    }

    /// "What affects this breeder?"
    pub fn causes_of(&self, receiver_id: &str) -> Vec<&crate::graph::CharacterizedEdge> {
        self.graph.edges_into(receiver_id)
    }

    /// "What does this breeder affect?"
    pub fn impact_of(&self, sender_id: &str) -> Vec<&crate::graph::CharacterizedEdge> {
        self.graph.edges_from(sender_id)
    }

    pub fn summary(&self) -> GraphSummary {
        self.graph.summary()
    }
}
