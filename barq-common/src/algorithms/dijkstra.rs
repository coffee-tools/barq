use std::collections::{HashMap, HashSet};
use crate::{
    graph::{Edge, NetworkGraph, Node},
    strategy::{Route, RouteHop, RouteInput, RouteOutput, Strategy},
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

fn find_min_distance(
    nodes: HashMap<String, Node>, 
    distance: &HashMap<String, u64>, 
    visited: &HashSet<String>
) -> String {
    let mut current_node_id = String::new();
    let mut current_node_distance = u64::MAX;

    for (pubkey, _) in nodes {
        match distance.get(&pubkey) {
            Some(&distance_value) => {
                if distance_value < current_node_distance && !visited.contains(&pubkey) {
                    current_node_distance = distance_value;
                    current_node_id = pubkey.clone();
                }
            },
            None => {
                println!("error finding minimum distance");
            }
        }
    }

    current_node_id
}

fn calculate_fee(
    amount_msat: u64, 
    edge: &Edge
) -> u64 {
    (edge.base_fee_millisatoshi + (amount_msat * edge.fee_per_millionth) / 1_000_000)
}

impl Strategy for Dijkstra {
    fn can_apply(&self, _input: &RouteInput) -> bool {
        // TODO: Implement the logic to check if the strategy can be applied to the given input

        true
    }

    fn route(&self, input: &RouteInput) -> RouteOutput {
        // TODO: Implement the routing logic
        let mut visited: HashSet<String> = HashSet::new();
        let mut distance: HashMap<String, u64> = HashMap::new();
        let mut route_paths: HashMap<String, Vec<Route>> = HashMap::new();

        for (pubkey, _) in input.graph.nodes.clone() {
            if pubkey == input.src_pubkey {
                distance.insert(pubkey.clone(), 0);
            } else {
                distance.insert(pubkey.clone(), u64::MAX);
            }
            route_paths.insert(pubkey.clone(), Vec::<Route>::new());
        }

        loop {
            let current_node_id = find_min_distance(input.graph.nodes.clone(), &distance, &visited);
            if visited.len() == input.graph.get_all_nodes().len() {
                break;
            }

            if let Some(node) = input.graph.get_node(&(current_node_id.clone())) {
                for channel in &node.channels {
                    if let Some(edge) = input.graph.get_edge(channel) {
                        let fee = calculate_fee(input.amount_msat, edge);
                        let current_node_distance = distance.get(&current_node_id).unwrap_or(&u64::MAX);
                        let destination = if edge.destination == current_node_id { edge.source.clone() } else { edge.destination.clone() }; 
                        let next_node_distance = *distance.get(&destination).unwrap_or(&u64::MAX);

                        if current_node_distance + fee < next_node_distance {
                            distance.insert(destination.clone(), current_node_distance + fee);
                            let mut route_path = route_paths.entry(current_node_id.clone()).or_insert_with(Vec::new).clone();
                            let route: Route = Route::new(destination.clone(), edge.id.clone(), edge.delay, fee);
                            route_path.push(route);
                            route_paths.insert(destination.clone(), route_path);
                        }
                    } else {
                        println!("Edge cannot be retrieved");
                    }
                }
            } else {
                println!("Node is absent");
            }

            visited.insert(current_node_id.clone());
        }

        let route_path = route_paths.entry(input.dest_pubkey.clone()).or_insert_with(Vec::new).clone();
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

        RouteOutput {
            path: path
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
        // TODO: write tests for dijsktra
    }
}
