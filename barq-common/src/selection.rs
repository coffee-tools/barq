//! Strategy ranking (issues #10 and #16).
//!
//! Every applicable strategy proposes a route; the routes are ranked so the
//! payment tries the best one first and falls back to the next on failure.
//!
//! The ranking metric is, in order:
//! 1. total routing fee (what the payer pays),
//! 2. total CLTV delta (how long funds can be locked if a hop stalls),
//! 3. number of hops (fewer nodes that can fail),
//! 4. the order the strategies were given in (cheaper strategies first).
//!
//! Two strategies proposing the same channels count once. A route that uses
//! a channel excluded by an earlier failed attempt is dropped even if its
//! strategy ignored the exclusion.
use crate::ldk::routing::router::Path;
use crate::strategy::{RouteInput, RouteOutput, Strategy, StrategyKind};

/// A strategy and the name it is reported under.
pub struct NamedStrategy {
    pub name: String,
    pub strategy: Box<dyn Strategy>,
}

impl NamedStrategy {
    pub fn new(name: impl Into<String>, strategy: Box<dyn Strategy>) -> Self {
        Self {
            name: name.into(),
            strategy,
        }
    }
}

/// A route proposed by a strategy.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub strategy: String,
    pub output: RouteOutput,
}

impl Candidate {
    pub fn fee_msat(&self) -> u64 {
        self.output.fee_msat()
    }

    pub fn total_cltv(&self) -> u32 {
        total_cltv(&self.output.path)
    }
}

/// A strategy that did not produce a route, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub strategy: String,
    pub reason: String,
}

/// The outcome of [`rank`].
#[derive(Debug, Clone, Default)]
pub struct Ranking {
    /// Best first.
    pub candidates: Vec<Candidate>,
    pub rejected: Vec<Rejected>,
}

impl Ranking {
    pub fn best(&self) -> Option<&Candidate> {
        self.candidates.first()
    }

    /// One line explaining why no strategy produced a route.
    pub fn explain(&self) -> String {
        self.rejected
            .iter()
            .map(|r| format!("{}: {}", r.strategy, r.reason))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

fn total_cltv(path: &Path) -> u32 {
    let clear: u32 = path.hops.iter().map(|h| h.cltv_expiry_delta).sum();
    clear
        + path
            .blinded_tail
            .as_ref()
            .map_or(0, |t| t.excess_final_cltv_expiry_delta)
}

fn channels(path: &Path) -> Vec<u64> {
    path.hops.iter().map(|h| h.short_channel_id).collect()
}

/// Ask every applicable strategy for a route and rank the routes.
///
/// When `input.strategy` forces a strategy, only that one is asked.
pub fn rank(strategies: &[NamedStrategy], input: &RouteInput) -> Ranking {
    let forced: Option<&str> = input.strategy.as_ref().map(StrategyKind::name);
    let mut ranking = Ranking::default();
    for named in strategies {
        if forced.is_some_and(|f| f != named.name) {
            continue;
        }
        let reject = |reason: String| Rejected {
            strategy: named.name.clone(),
            reason,
        };
        match named.strategy.can_apply(input) {
            Ok(true) => {}
            Ok(false) => {
                ranking.rejected.push(reject(format!(
                    "Strategy `{}` cannot route this payment",
                    named.name
                )));
                continue;
            }
            Err(err) => {
                ranking.rejected.push(reject(format!("{err}")));
                continue;
            }
        }
        match named.strategy.route(input) {
            Ok(output) if output.path.hops.is_empty() => {
                ranking.rejected.push(reject("empty route".to_owned()))
            }
            Ok(output) => {
                if let Some(scid) = channels(&output.path)
                    .into_iter()
                    .find(|scid| input.excluded_channels.contains(scid))
                {
                    ranking.rejected.push(reject(format!(
                        "route uses channel {} which already failed",
                        crate::scid::to_cln(scid)
                    )));
                    continue;
                }
                ranking.candidates.push(Candidate {
                    strategy: named.name.clone(),
                    output,
                });
            }
            Err(err) => ranking.rejected.push(reject(format!("{err}"))),
        }
    }
    // Stable: equal routes keep the strategy order.
    ranking
        .candidates
        .sort_by_key(|c| (c.fee_msat(), c.total_cltv(), c.output.path.hops.len()));
    let mut seen: Vec<Vec<u64>> = vec![];
    ranking.candidates.retain(|c| {
        let key = channels(&c.output.path);
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    ranking
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use anyhow::Result;

    use super::*;
    use crate::algorithms::direct::Direct;
    use crate::algorithms::probabilistic::LDKRoutingStrategy;
    use crate::ldk::ln::features::{ChannelFeatures, NodeFeatures};
    use crate::ldk::routing::router::RouteHop;
    use crate::secp256k1::PublicKey;
    use crate::simulation::{Policy, SimulatedNetwork};

    /// A strategy that returns a fixed route, or fails.
    struct Fixed {
        applies: bool,
        route: Option<Vec<(u64, u64, u32)>>, // (scid, fee, cltv) per hop
    }

    impl Strategy for Fixed {
        fn can_apply(&self, _: &RouteInput) -> Result<bool> {
            Ok(self.applies)
        }

        fn route(&self, _: &RouteInput) -> Result<RouteOutput> {
            let Some(hops) = &self.route else {
                anyhow::bail!("no route for you");
            };
            let pubkey = PublicKey::from_str(
                "02eec7245d6b7d2ccb30380bfbe2a3648cd7a942653f5aa340edcea1f283686619",
            )?;
            Ok(RouteOutput {
                path: Path {
                    hops: hops
                        .iter()
                        .map(|&(scid, fee, cltv)| RouteHop {
                            pubkey,
                            node_features: NodeFeatures::empty(),
                            short_channel_id: scid,
                            channel_features: ChannelFeatures::empty(),
                            fee_msat: fee,
                            cltv_expiry_delta: cltv,
                            maybe_announced_channel: true,
                        })
                        .collect(),
                    blinded_tail: None,
                },
            })
        }
    }

    fn fixed(name: &str, route: Option<Vec<(u64, u64, u32)>>) -> NamedStrategy {
        NamedStrategy::new(
            name,
            Box::new(Fixed {
                applies: true,
                route,
            }),
        )
    }

    fn input() -> RouteInput {
        SimulatedNetwork::line(2, 1_000_000, Policy::default())
            .unwrap()
            .route_input(0, 1, 1_000)
    }

    // The last hop's fee is the amount delivered, so it is not part of the
    // routing fee: give every route the same last hop amount.
    #[test]
    fn ranks_by_fee_then_cltv_then_hops_then_order() {
        let strategies = vec![
            fixed("expensive", Some(vec![(1, 500, 40), (2, 1000, 18)])),
            fixed("slow", Some(vec![(3, 100, 400), (4, 1000, 18)])),
            fixed("long", Some(vec![(5, 50, 10), (6, 50, 10), (7, 1000, 18)])),
            fixed("best", Some(vec![(8, 100, 40), (9, 1000, 18)])),
        ];
        let ranking = rank(&strategies, &input());
        let order: Vec<_> = ranking
            .candidates
            .iter()
            .map(|c| c.strategy.as_str())
            .collect();
        // long: fee 100, cltv 38. best: fee 100, cltv 58. slow: fee 100,
        // cltv 418. expensive: fee 500.
        assert_eq!(order, vec!["long", "best", "slow", "expensive"]);
        assert!(ranking.rejected.is_empty());
    }

    #[test]
    fn equal_routes_keep_strategy_order_and_are_deduplicated() {
        let strategies = vec![
            fixed("first", Some(vec![(1, 1000, 18)])),
            fixed("second", Some(vec![(1, 1000, 18)])),
        ];
        let ranking = rank(&strategies, &input());
        assert_eq!(ranking.candidates.len(), 1);
        assert_eq!(ranking.candidates[0].strategy, "first");
    }

    #[test]
    fn failures_and_inapplicable_strategies_are_reported() {
        let strategies = vec![
            NamedStrategy::new(
                "skip",
                Box::new(Fixed {
                    applies: false,
                    route: None,
                }),
            ),
            fixed("broken", None),
            fixed("works", Some(vec![(1, 1000, 18)])),
        ];
        let ranking = rank(&strategies, &input());
        assert_eq!(ranking.best().unwrap().strategy, "works");
        assert_eq!(
            ranking.rejected,
            vec![
                Rejected {
                    strategy: "skip".into(),
                    reason: "Strategy `skip` cannot route this payment".into()
                },
                Rejected {
                    strategy: "broken".into(),
                    reason: "no route for you".into()
                },
            ]
        );
        assert!(ranking.explain().contains("broken: no route for you"));
    }

    #[test]
    fn routes_through_excluded_channels_are_dropped() {
        let strategies = vec![
            fixed("uses-bad", Some(vec![(66, 0, 18)])),
            fixed("fine", Some(vec![(1, 1000, 18)])),
        ];
        let mut input = input();
        input.excluded_channels = vec![66];
        let ranking = rank(&strategies, &input);
        assert_eq!(ranking.candidates.len(), 1);
        assert_eq!(ranking.best().unwrap().strategy, "fine");
        assert!(ranking.rejected[0].reason.contains("already failed"));
    }

    fn builtin(net: &SimulatedNetwork) -> Vec<NamedStrategy> {
        vec![
            NamedStrategy::new("direct", Box::new(Direct::new())),
            NamedStrategy::new(
                "probabilistic",
                Box::new(LDKRoutingStrategy::new(net.ctx.clone())),
            ),
        ]
    }

    #[test]
    fn forced_strategy_is_the_only_one_tried() {
        // Issue #10: a peer, so both strategies apply.
        let net = SimulatedNetwork::line(3, 1_000_000, Policy::default()).unwrap();
        let mut input = net.route_input(0, 1, 10_000);

        let auto = rank(&builtin(&net), &input);
        // Same single-channel path from both: deduplicated, direct first.
        assert_eq!(auto.candidates.len(), 1);
        assert_eq!(auto.best().unwrap().strategy, "direct");

        input.strategy = Some(StrategyKind::Probabilistic);
        let forced = rank(&builtin(&net), &input);
        assert_eq!(forced.best().unwrap().strategy, "probabilistic");
        assert!(forced.rejected.is_empty());

        // Forcing a strategy that cannot route gives no fallback.
        let mut input = net.route_input(0, 2, 10_000);
        input.strategy = Some(StrategyKind::Direct);
        let forced = rank(&builtin(&net), &input);
        assert!(forced.candidates.is_empty());
        assert_eq!(forced.rejected.len(), 1);
    }

    #[test]
    fn falls_back_when_the_direct_channel_is_too_small() {
        // Issue #16: 0 -- 2 directly through a small channel, and
        // 0 -- 1 -- 2 through big ones.
        let mut net = SimulatedNetwork::new().unwrap();
        net.add_nodes(3);
        let p = Policy::default();
        net.add_channel(0, 2, 10_000, p, p).unwrap();
        net.add_channel(0, 1, 1_000_000, p, p).unwrap();
        net.add_channel(1, 2, 1_000_000, p, p).unwrap();

        let ranking = rank(&builtin(&net), &net.route_input(0, 2, 50_000_000));
        assert_eq!(ranking.best().unwrap().strategy, "probabilistic");
        assert_eq!(
            ranking.best().unwrap().output.path.hops[0].pubkey,
            net.node_id(1)
        );
        assert!(ranking.rejected[0].reason.contains("can carry"));
    }

    #[test]
    fn excluding_a_failed_channel_moves_to_the_next_path() {
        // Issue #16: after the cheap path fails, the expensive one is used.
        let mut net = SimulatedNetwork::new().unwrap();
        net.add_nodes(4);
        let cheap = Policy::with_fees(0, 1);
        let pricey = Policy::with_fees(10_000, 100);
        net.add_channel(0, 1, 1_000_000, cheap, cheap).unwrap();
        let failing = net.add_channel(1, 3, 1_000_000, cheap, cheap).unwrap();
        net.add_channel(0, 2, 1_000_000, pricey, pricey).unwrap();
        net.add_channel(2, 3, 1_000_000, pricey, pricey).unwrap();

        let mut input = net.route_input(0, 3, 100_000);
        let first = rank(&builtin(&net), &input);
        assert_eq!(
            first.best().unwrap().output.path.hops[0].pubkey,
            net.node_id(1)
        );

        input.excluded_channels.push(failing);
        let second = rank(&builtin(&net), &input);
        let best = second.best().unwrap();
        assert_eq!(best.output.path.hops[0].pubkey, net.node_id(2));
        assert!(best.fee_msat() > first.best().unwrap().fee_msat());

        // Both paths excluded: nothing left.
        input
            .excluded_channels
            .push(best.output.path.hops[1].short_channel_id);
        assert!(rank(&builtin(&net), &input).candidates.is_empty());
    }

    #[test]
    fn direct_skips_an_excluded_channel_to_the_same_peer() {
        let mut net = SimulatedNetwork::new().unwrap();
        net.add_nodes(2);
        let p = Policy::default();
        let first = net.add_channel(0, 1, 1_000_000, p, p).unwrap();
        let second = net.add_channel(0, 1, 1_000_000, p, p).unwrap();
        let mut input = net.route_input(0, 1, 10_000);
        input.strategy = Some(StrategyKind::Direct);
        input.excluded_channels = vec![first];
        let ranking = rank(&builtin(&net), &input);
        assert_eq!(
            ranking.best().unwrap().output.path.hops[0].short_channel_id,
            second
        );
    }
}
