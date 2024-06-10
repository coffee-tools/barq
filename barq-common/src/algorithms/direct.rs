use crate::{
    strategy::{RouteInput, RouteOutput, Strategy},
};

pub struct Direct;

impl Default for Direct {
    fn default() -> Self {
        Self::new()
    }
}

impl Direct {
    pub fn new() -> Self {
        Direct
    }
}

impl Strategy for Direct {
    fn can_apply(&self, _input: &RouteInput) -> bool {
        // TODO: Implement the logic to check if the strategy can be applied to the given input

        true
    }

    fn route(&self, input: &RouteInput) -> RouteOutput {
        // TODO: Implement the routing logic

        RouteOutput {
            path: vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::Router;

    #[test]
    fn test_direct_routing() {
        let router = Router::new(vec![Box::new(Direct::new())]);
        let input = RouteInput {
            src_pubkey: "A".to_string(),
            dest_pubkey: "B".to_string(),
            amount_msat: 100,
            graph: NetworkGraph::new(),
        };
        let output = router.execute(&input);
        // assert_eq!(output.path, vec!["A", "B"]);
        // assert_eq!(output.total_fees, 0);
    }
}
