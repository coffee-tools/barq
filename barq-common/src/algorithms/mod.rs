pub mod direct;
pub mod probabilistic;

use crate::strategy::StrategyKind;

use super::strategy::Strategy;

pub fn get_algorithm(strategy: &StrategyKind) -> Option<Box<dyn Strategy>> {
    match strategy {
        StrategyKind::Direct => Some(Box::new(direct::Direct::new())),
        #[allow(clippy::box_default)]
        StrategyKind::Probabilistic => Some(Box::new(probabilistic::LDKRoutingStrategy::default())),
    }
}
