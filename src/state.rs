use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Addr, Uint128};
use cw_storage_plus::{Item, Map};

#[cw_serde]
pub struct Config {
    pub admin: Addr,
    pub attestor: Addr,
    /// Receives the remainder of every payment after the pool share.
    pub treasury: Addr,
    /// Receives the `pool_bps` share of every payment, e.g. the weekly draw pool.
    pub pool: Addr,
    pub half_life_secs: u64,
    pub epoch_len_secs: u64,
    pub epoch_zero: u64,
    pub max_delta: Uint128,
    pub seeding: bool,
    pub paused: bool,
}

#[cw_serde]
pub struct WeightTier {
    /// Applies while the occurrence count for this ref_id is strictly below it.
    pub up_to: u64,
    pub weight: Uint128,
}

#[cw_serde]
pub struct ActionParams {
    /// Weight applied once every tier is exhausted. Also the only weight
    /// when `tiers` is empty.
    pub weight: Uint128,
    /// Descending schedule keyed by how many times this action has already
    /// been recorded against the same ref_id, across all users.
    pub tiers: Vec<WeightTier>,
    /// Required payment in uluna. Zero means the action is free
    /// and can only be recorded by the attestor.
    pub price: Uint128,
    /// Fixed amount routed to the pool address; whatever is left goes to the
    /// treasury. Fixed rather than a percentage because rank discounts must
    /// reduce only the treasury leg — the pool leg backs draw entries at a
    /// protocol-wide rate and has to stay exact regardless of who is paying.
    pub pool_amount: Uint128,
    /// Max occurrences per user per ref_id per day. Zero means unlimited.
    pub daily_limit: u8,
}

#[cw_serde]
pub struct ScoreEntry {
    /// Score as of `last_update`. Decay is applied lazily on read or write.
    pub raw: Uint128,
    pub last_update: u64,
    /// Cumulative total ever granted, never decayed. History only.
    pub lifetime_earned: Uint128,
    pub actions: u64,
}

pub const CONFIG: Item<Config> = Item::new("config");
pub const ACTIONS: Map<&str, ActionParams> = Map::new("actions");
pub const SCORES: Map<&Addr, ScoreEntry> = Map::new("scores");
pub const EPOCH_SCORES: Map<(u64, &Addr), Uint128> = Map::new("epoch_scores");
/// Keyed day-first so stale entries sort to the front and can be pruned
/// with a bounded range scan instead of a full table walk.
pub const RATE_LIMIT: Map<(u64, &Addr, &str), u8> = Map::new("rate_limit_v2");
/// Occurrences per (UTC day, action, user). Drives the tiered weight schedule:
/// the ladder caps how much one person earns per day across all references,
/// so a popular question never penalises whoever answers it late.
/// Day-first for the same reason as RATE_LIMIT — stale rows prune cheaply.
pub const TIER_COUNT: Map<(u64, &str, &Addr), u64> = Map::new("tier_count");

/// Weight for the next occurrence, given how many came before it.
pub fn resolve_weight(params: &ActionParams, prior_count: u64) -> Uint128 {
    for tier in params.tiers.iter() {
        if prior_count < tier.up_to {
            return tier.weight;
        }
    }
    params.weight
}

/// Exponential decay with a configurable half-life.
///
/// Whole half-lives are applied by successive halving; the leftover fraction
/// uses a linear approximation. Peak error is about 6% mid-interval and always
/// resolves in the user's favour, never against them.
///
/// This is deliberate: constant gas cost instead of iterating day by day, which
/// would be unbounded for long-dormant accounts.
pub fn decay(raw: Uint128, dt: u64, half_life_secs: u64) -> Uint128 {
    if raw.is_zero() || dt == 0 || half_life_secs == 0 {
        return raw;
    }
    let periods = dt / half_life_secs;
    if periods >= 128 {
        return Uint128::zero();
    }
    let mut v = raw;
    for _ in 0..periods {
        v = v / Uint128::new(2);
        if v.is_zero() {
            return v;
        }
    }
    let rem = dt % half_life_secs;
    let den = 2u128 * half_life_secs as u128;
    let num = den - rem as u128;
    v.multiply_ratio(num, den)
}

/// Epoch index counted from contract instantiation.
pub fn epoch_of(cfg: &Config, now: u64) -> u64 {
    if cfg.epoch_len_secs == 0 {
        return 0;
    }
    now.saturating_sub(cfg.epoch_zero) / cfg.epoch_len_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 90-day half-life, in seconds.
    const HL: u64 = 90 * 86_400;

    fn test_cfg() -> Config {
        Config {
            admin: Addr::unchecked("admin"),
            attestor: Addr::unchecked("attestor"),
            treasury: Addr::unchecked("treasury"),
            pool: Addr::unchecked("pool"),
            half_life_secs: HL,
            epoch_len_secs: 7 * 86_400,
            epoch_zero: 1_000_000,
            max_delta: Uint128::new(100),
            seeding: true,
            paused: false,
        }
    }

    fn tiered() -> ActionParams {
        ActionParams {
            weight: Uint128::zero(),
            tiers: vec![
                WeightTier {
                    up_to: 3,
                    weight: Uint128::new(40),
                },
                WeightTier {
                    up_to: 10,
                    weight: Uint128::new(10),
                },
            ],
            price: Uint128::zero(),
            pool_amount: Uint128::zero(),
            daily_limit: 0,
        }
    }

    #[test]
    fn no_elapsed_time_means_no_decay() {
        assert_eq!(decay(Uint128::new(1000), 0, HL), Uint128::new(1000));
    }

    #[test]
    fn one_half_life_halves_the_score() {
        assert_eq!(decay(Uint128::new(1000), HL, HL), Uint128::new(500));
    }

    #[test]
    fn two_half_lives_quarter_the_score() {
        assert_eq!(decay(Uint128::new(1000), 2 * HL, HL), Uint128::new(250));
    }

    #[test]
    fn partial_interval_uses_the_linear_approximation() {
        // Exact value is 1000 * 2^-0.5 = 707. The approximation yields 750,
        // about 6% high and in the user's favour. Known tradeoff, not a bug.
        assert_eq!(decay(Uint128::new(1000), HL / 2, HL), Uint128::new(750));
    }

    #[test]
    fn decay_never_increases_over_time() {
        let start = Uint128::new(1_000_000);
        let mut prev = start;
        for day in 1..=400u64 {
            let v = decay(start, day * 86_400, HL);
            assert!(v <= prev, "score went up on day {}", day);
            prev = v;
        }
    }

    #[test]
    fn long_dormancy_falls_to_zero() {
        assert_eq!(
            decay(Uint128::new(1_000_000_000), 200 * HL, HL),
            Uint128::zero()
        );
    }

    #[test]
    fn zero_score_stays_zero() {
        assert_eq!(decay(Uint128::zero(), 10 * HL, HL), Uint128::zero());
    }

    #[test]
    fn zero_half_life_disables_decay() {
        assert_eq!(decay(Uint128::new(1000), 10 * HL, 0), Uint128::new(1000));
    }

    #[test]
    fn epoch_advances_on_the_week_boundary() {
        let cfg = test_cfg();
        let week = 7 * 86_400;
        assert_eq!(epoch_of(&cfg, cfg.epoch_zero), 0);
        assert_eq!(epoch_of(&cfg, cfg.epoch_zero + week - 1), 0);
        assert_eq!(epoch_of(&cfg, cfg.epoch_zero + week), 1);
        assert_eq!(epoch_of(&cfg, cfg.epoch_zero + 3 * week), 3);
    }

    #[test]
    fn timestamps_before_instantiation_clamp_to_epoch_zero() {
        let cfg = test_cfg();
        assert_eq!(epoch_of(&cfg, cfg.epoch_zero - 500), 0);
    }

    #[test]
    fn tiers_step_down_at_the_declared_boundaries() {
        let p = tiered();
        // Answers 1-3 sit at the top tier.
        assert_eq!(resolve_weight(&p, 0), Uint128::new(40));
        assert_eq!(resolve_weight(&p, 2), Uint128::new(40));
        // Answers 4-10 drop to the second tier.
        assert_eq!(resolve_weight(&p, 3), Uint128::new(10));
        assert_eq!(resolve_weight(&p, 9), Uint128::new(10));
        // Answer 11 onwards falls through to the base weight.
        assert_eq!(resolve_weight(&p, 10), Uint128::zero());
        assert_eq!(resolve_weight(&p, 9_999), Uint128::zero());
    }

    #[test]
    fn empty_tiers_always_use_the_base_weight() {
        let p = ActionParams {
            weight: Uint128::new(40),
            tiers: vec![],
            price: Uint128::zero(),
            pool_amount: Uint128::zero(),
            daily_limit: 0,
        };
        assert_eq!(resolve_weight(&p, 0), Uint128::new(40));
        assert_eq!(resolve_weight(&p, 500), Uint128::new(40));
    }
}
