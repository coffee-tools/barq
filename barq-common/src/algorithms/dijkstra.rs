use crate::{
    graph::{Edge, Node},
    strategy::{Route, RouteHop, RouteInput, RouteOutput, Strategy},
};
use std::collections::{HashMap, HashSet};

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

fn find_min_distance(
    nodes: Vec<&Node>,
    distance: &HashMap<String, u64>,
    visited: &HashSet<String>,
) -> String {
    let mut current_node_id = String::new();
    let mut current_node_distance = &u64::MAX;

    for node in nodes {
        let distance_value = distance.get(&node.id).expect(&format!("Cannot retrive distance of node with id {}", current_node_id));
            if distance_value < current_node_distance && !visited.contains(&node.id) {
                current_node_distance = distance_value;
                current_node_id = node.id.clone();
            }
        }

    return current_node_id;
}

fn calculate_fee(amount_msat: u64, edge: &Edge) -> u64 {
    edge.base_fee_millisatoshi + (amount_msat * edge.fee_per_millionth) / 1_000_000
}

impl Strategy for Dijkstra {
    fn can_apply(&self, _input: &RouteInput) -> bool {
        // TODO: Implement the logic to check if the strategy can be applied to the given input

        true
    }

    fn route(&self, input: &RouteInput) -> Result<RouteOutput, String> {
        // TODO: Implement the routing logic
        let mut visited: HashSet<String> = HashSet::new();
        let mut distance: HashMap<String, u64> = HashMap::new();
        let mut route_paths: HashMap<String, Vec<Route>> = HashMap::new();

        for node in input.graph.get_all_nodes() {
            if node.id == input.src_pubkey {
                distance.insert(node.id.clone(), 0);
            } else {
                distance.insert(node.id.clone(), u64::MAX);
            }
            route_paths.insert(node.id.clone(), Vec::<Route>::new());
        }

        loop {
            let current_node_id = find_min_distance(input.graph.get_all_nodes(), &distance, &visited);
            if visited.len() == input.graph.get_all_nodes().len() {
                break;
            }

            let node = input.graph.get_node(&(current_node_id.clone())).expect(
                &format!("Node {} cannot be retrived from Network Graph", current_node_id.clone())
            );
            for channel in &node.channels {
                let edge = input.graph.get_edge(channel).expect(
                    &format!("Edge {} cannot be retrived from Network Graph", channel.clone())
                );

                let fee = calculate_fee(input.amount_msat, edge);
                let current_node_distance =
                    distance.get(&current_node_id).unwrap_or(&u64::MAX);
                let destination = if edge.node1 == current_node_id {
                    edge.node1.clone()
                } else {
                    edge.node2.clone()
                };
                let next_node_distance = *distance.get(&destination).unwrap_or(&u64::MAX);

                if current_node_distance + fee < next_node_distance {
                    distance.insert(destination.clone(), current_node_distance + fee);
                    let mut route_path = route_paths
                        .entry(current_node_id.clone())
                        .or_insert_with(Vec::new)
                        .clone();
                    let route: Route =
                        Route::new(destination.clone(), edge.id.clone(), edge.delay, fee);
                    route_path.push(route);
                    route_paths.insert(destination.clone(), route_path);
                }
            }

            visited.insert(current_node_id.clone());
        }

        let route_path = route_paths
            .entry(input.dest_pubkey.clone())
            .or_insert_with(Vec::new)
            .clone();
        let mut path: Vec<RouteHop> = Vec::<RouteHop>::new();
        let mut total_amt: u64 = input.amount_msat;
        let mut total_delay: u64 = 9;
        for i in 0..(route_path.len() - 1) {
            total_amt += route_path[i].fee;
            total_delay += route_path[i].delay;
        }

        for route in route_path {
            let hop = RouteHop::new(route.id, route.channel, total_delay, total_amt);
            path.push(hop);
            total_amt -= route.fee;
            total_delay -= route.delay;
        }

        Ok(RouteOutput { path: path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::Router;

    #[test]
    fn test_dijkstra_routing() {
        let mut strategies: Vec<Box<dyn Strategy>> = Vec::new();
        strategies.push(Box::new(Dijkstra::new()));
        let router = Router::new(strategies);
        // TODO: write tests for dijsktra
    }
}
