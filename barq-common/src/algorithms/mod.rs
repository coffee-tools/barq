pub mod dijkstra;
pub mod direct;
pub mod ldk_router;

use super::strategy::Strategy;

pub fn get_algorithm(name: &str) -> Option<Box<dyn Strategy>> {
    match name {
        "direct" => Some(Box::new(direct::Direct::new())),
        "dijkstra" => Some(Box::new(dijkstra::Dijkstra::new())),
        _ => None,
    }
}
