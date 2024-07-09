pub mod direct;
pub mod probabilistic;

use super::strategy::Strategy;

pub fn get_algorithm(name: &str) -> Option<Box<dyn Strategy>> {
    match name {
        "direct" => Some(Box::new(direct::Direct::new())),
        "probabilistic" => Some(Box::new(probabilistic::LDKRoutingStrategy::default())),
        _ => None,
    }
}
