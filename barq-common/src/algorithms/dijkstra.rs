use crate::{
    strategy::{RouteInput, RouteOutput, Strategy},
};

pub struct Dijkstra;

impl Default for Dijkstra {
    fn default() -> Self {
        Self::new()
    }
}

impl Dijkstra {
    pub fn new() -> Self {
        Dijkstra
    }
}

impl Strategy for Dijkstra {
    fn can_apply(&self, _input: &RouteInput) -> bool {
        // TODO: Implement the logic to check if the strategy can be applied to the given input

        true
    }

    fn route(&self, input: &RouteInput) -> RouteOutput {
        // TODO: Implement the routing logic

        RouteOutput {
            path: vec![input.source.clone(), input.destination.clone()],
            total_fees: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::Router;

    #[test]
    fn test_dijkstra_routing() {
        let router = Router::new(vec![Box::new(Dijkstra::new())]);
        let input = RouteInput {
            source: "A".to_string(),
            destination: "B".to_string(),
            amount: 100,
            graph: NetworkGraph::new(),
        };
        let output = router.execute(&input);
        assert_eq!(output.path, vec!["A", "B"]);
        assert_eq!(output.total_fees, 0);
    }
}
