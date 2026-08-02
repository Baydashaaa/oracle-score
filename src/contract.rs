#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    coins, to_json_binary, Addr, BankMsg, Binary, Deps, DepsMut, Env, MessageInfo, Order, Response,
    StdResult, Storage, Uint128,
};
use cw2::set_contract_version;
use cw_storage_plus::Bound;

use crate::error::ContractError;
use crate::msg::*;
use crate::state::*;

const CONTRACT_NAME: &str = "crates.io:oracle-score";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");
const DAY: u64 = 86_400;
const DENOM: &str = "uluna";
fn validate_params(p: &ActionParams) -> Result<(), ContractError> {
    // A payment of exactly `price` must always cover the pool leg. Without this
    // the cheapest valid payment would leave a draw entry partly unbacked.
    if p.pool_amount > p.price {
        return Err(ContractError::PoolExceedsPrice {});
    }
    Ok(())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let admin = match msg.admin {
        Some(a) => deps.api.addr_validate(&a)?,
        None => info.sender.clone(),
    };

    let cfg = Config {
        admin,
        attestor: deps.api.addr_validate(&msg.attestor)?,
        treasury: deps.api.addr_validate(&msg.treasury)?,
        pool: deps.api.addr_validate(&msg.pool)?,
        half_life_secs: msg.half_life_days * DAY,
        epoch_len_secs: msg.epoch_len_days * DAY,
        epoch_zero: env.block.time.seconds(),
        max_delta: msg.max_delta,
        seeding: true,
        paused: false,
    };
    CONFIG.save(deps.storage, &cfg)?;

    for item in msg.actions {
        validate_params(&item.params)?;
        ACTIONS.save(deps.storage, item.key.as_str(), &item.params)?;
    }

    Ok(Response::new().add_attribute("action", "instantiate"))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::PaidAction { action, ref_id } => {
            exec_paid_action(deps, env, info, action, ref_id)
        }
        ExecuteMsg::RecordAction {
            user,
            action,
            ref_id,
            amount,
        } => exec_record_action(deps, env, info, user, action, ref_id, amount),
        ExecuteMsg::SeedScores { entries } => exec_seed(deps, env, info, entries),
        ExecuteMsg::FinalizeSeeding {} => exec_finalize_seeding(deps, info),
        ExecuteMsg::Slash {
            user,
            amount,
            reason,
        } => exec_slash(deps, env, info, user, amount, reason),
        ExecuteMsg::PruneRateLimit { before_day, limit } => {
            exec_prune_rate_limit(deps, info, before_day, limit)
        }
        ExecuteMsg::SetAction { item } => exec_set_action(deps, info, item),
        ExecuteMsg::UpdateConfig {
            attestor,
            treasury,
            pool,
            max_delta,
            half_life_days,
        } => exec_update_config(deps, info, attestor, treasury, pool, max_delta, half_life_days),
        ExecuteMsg::SetAdmin { admin } => exec_set_admin(deps, info, admin),
        ExecuteMsg::SetPaused { paused } => exec_set_paused(deps, info, paused),
    }
}

fn accrue(
    storage: &mut dyn Storage,
    cfg: &Config,
    now: u64,
    user: &Addr,
    amount: Uint128,
) -> StdResult<Uint128> {
    let mut e = SCORES
        .may_load(storage, user)?
        .unwrap_or(ScoreEntry {
            raw: Uint128::zero(),
            last_update: now,
            lifetime_earned: Uint128::zero(),
            actions: 0,
        });

    let dt = now.saturating_sub(e.last_update);
    e.raw = decay(e.raw, dt, cfg.half_life_secs) + amount;
    e.last_update = now;
    e.lifetime_earned += amount;
    e.actions += 1;
    SCORES.save(storage, user, &e)?;

    let ep = epoch_of(cfg, now);
    EPOCH_SCORES.update(storage, (ep, user), |v| -> StdResult<Uint128> {
        Ok(v.unwrap_or_default() + amount)
    })?;

    Ok(e.raw)
}

fn check_rate_limit(
    storage: &mut dyn Storage,
    now: u64,
    user: &Addr,
    ref_id: &str,
    limit: u8,
) -> Result<(), ContractError> {
    if limit == 0 {
        return Ok(());
    }
    let day = now / DAY;
    let key = (day, user, ref_id);
    let used = RATE_LIMIT.may_load(storage, key)?.unwrap_or(0);
    if used >= limit {
        return Err(ContractError::RateLimited {});
    }
    RATE_LIMIT.save(storage, key, &(used + 1))?;
    Ok(())
}

/// Returns how many times this user already did this action today,
/// then records the current one.
fn bump_tier_count(
    storage: &mut dyn Storage,
    now: u64,
    action: &str,
    user: &Addr,
) -> StdResult<u64> {
    let key = (now / DAY, action, user);
    let prior = TIER_COUNT.may_load(storage, key)?.unwrap_or(0);
    TIER_COUNT.save(storage, key, &(prior + 1))?;
    Ok(prior)
}

fn exec_paid_action(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    action: String,
    ref_id: String,
) -> Result<Response, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if cfg.paused {
        return Err(ContractError::Paused {});
    }

    let params = ACTIONS
        .may_load(deps.storage, action.as_str())?
        .ok_or(ContractError::UnknownAction {
            action: action.clone(),
        })?;

    if params.price.is_zero() {
        return Err(ContractError::NotPayable {});
    }
    if info.funds.iter().any(|c| c.denom != DENOM) {
        return Err(ContractError::UnexpectedDenom {});
    }

    let paid = info
        .funds
        .iter()
        .fold(Uint128::zero(), |acc, c| acc + c.amount);

    if paid < params.price {
        return Err(ContractError::InsufficientPayment { need: params.price });
    }

    let now = env.block.time.seconds();
    check_rate_limit(deps.storage, now, &info.sender, &ref_id, params.daily_limit)?;
    let prior = bump_tier_count(deps.storage, now, &action, &info.sender)?;
    let weight = resolve_weight(&params, prior);
    let new_raw = accrue(deps.storage, &cfg, now, &info.sender, weight)?;

    // validate_params guarantees pool_amount <= price and paid >= price was
    // checked above, so this cannot underflow. Discounts and overpayment both
    // land entirely on the treasury leg — the pool leg is fixed by the tariff.
    let to_pool = params.pool_amount;
    let to_treasury = paid - to_pool;

    let mut msgs: Vec<BankMsg> = vec![];
    if !to_pool.is_zero() {
        msgs.push(BankMsg::Send {
            to_address: cfg.pool.to_string(),
            amount: coins(to_pool.u128(), DENOM),
        });
    }
    if !to_treasury.is_zero() {
        msgs.push(BankMsg::Send {
            to_address: cfg.treasury.to_string(),
            amount: coins(to_treasury.u128(), DENOM),
        });
    }

    Ok(Response::new()
        .add_messages(msgs)
        .add_attribute("action", "paid_action")
        .add_attribute("kind", action)
        .add_attribute("ref_id", ref_id)
        .add_attribute("user", info.sender.to_string())
        .add_attribute("granted", weight.to_string())
        .add_attribute("to_pool", to_pool.to_string())
        .add_attribute("to_treasury", to_treasury.to_string())
        .add_attribute("raw", new_raw.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn exec_record_action(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    user: String,
    action: String,
    ref_id: String,
    amount: Option<Uint128>,
) -> Result<Response, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if cfg.paused {
        return Err(ContractError::Paused {});
    }
    if info.sender != cfg.attestor {
        return Err(ContractError::Unauthorized {});
    }

    let params = ACTIONS
        .may_load(deps.storage, action.as_str())?
        .ok_or(ContractError::UnknownAction {
            action: action.clone(),
        })?;

    // The attestor may only touch free actions, unless this one is explicitly
    // marked otherwise in its config.
    if !params.price.is_zero() && !params.attestor_may_record {
        return Err(ContractError::NotFree {});
    }

    let user_addr = deps.api.addr_validate(&user)?;
    let now = env.block.time.seconds();
    check_rate_limit(deps.storage, now, &user_addr, &ref_id, params.daily_limit)?;
    let prior = bump_tier_count(deps.storage, now, &action, &user_addr)?;
    // A supplied amount replaces the tiered weight, but only where the action
    // was configured to allow it. Everywhere else the config decides, not the
    // caller — that is what keeps the attestor key from choosing its own grants.
    let weight = match amount {
        Some(a) => {
            if !params.variable_amount {
                return Err(ContractError::AmountNotAllowed {});
            }
            a
        }
        None => resolve_weight(&params, prior),
    };

    if weight > cfg.max_delta {
        return Err(ContractError::DeltaTooLarge {});
    }

    let new_raw = accrue(deps.storage, &cfg, now, &user_addr, weight)?;

    Ok(Response::new()
        .add_attribute("action", "record_action")
        .add_attribute("kind", action)
        .add_attribute("ref_id", ref_id)
        .add_attribute("user", user)
        .add_attribute("granted", weight.to_string())
        .add_attribute("raw", new_raw.to_string()))
}

fn exec_seed(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    entries: Vec<SeedEntry>,
) -> Result<Response, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if info.sender != cfg.admin {
        return Err(ContractError::Unauthorized {});
    }
    if !cfg.seeding {
        return Err(ContractError::SeedingClosed {});
    }

    let now = env.block.time.seconds();
    let count = entries.len();
    for e in entries {
        let addr = deps.api.addr_validate(&e.user)?;
        SCORES.save(
            deps.storage,
            &addr,
            &ScoreEntry {
                raw: e.raw,
                last_update: now,
                lifetime_earned: e.raw,
                actions: 0,
            },
        )?;
    }

    Ok(Response::new()
        .add_attribute("action", "seed_scores")
        .add_attribute("count", count.to_string()))
}

fn exec_finalize_seeding(deps: DepsMut, info: MessageInfo) -> Result<Response, ContractError> {
    let mut cfg = CONFIG.load(deps.storage)?;
    if info.sender != cfg.admin {
        return Err(ContractError::Unauthorized {});
    }
    if !cfg.seeding {
        return Err(ContractError::SeedingClosed {});
    }
    cfg.seeding = false;
    CONFIG.save(deps.storage, &cfg)?;
    Ok(Response::new().add_attribute("action", "finalize_seeding"))
}

fn exec_slash(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    user: String,
    amount: Uint128,
    reason: String,
) -> Result<Response, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if info.sender != cfg.admin {
        return Err(ContractError::Unauthorized {});
    }

    let addr = deps.api.addr_validate(&user)?;
    let now = env.block.time.seconds();
    let mut e = SCORES.load(deps.storage, &addr)?;
    let dt = now.saturating_sub(e.last_update);
    let current = decay(e.raw, dt, cfg.half_life_secs);
    e.raw = current.saturating_sub(amount);
    // Rank reads lifetime_earned, so slashing only the decaying figure would
    // strip an abuser's weight while leaving their rank and fee discount intact.
    e.lifetime_earned = e.lifetime_earned.saturating_sub(amount);
    e.last_update = now;
    SCORES.save(deps.storage, &addr, &e)?;

    Ok(Response::new()
        .add_attribute("action", "slash")
        .add_attribute("user", user)
        .add_attribute("amount", amount.to_string())
        .add_attribute("reason", reason)
        .add_attribute("raw", e.raw.to_string()))
}

fn exec_prune_rate_limit(
    deps: DepsMut,
    info: MessageInfo,
    before_day: u64,
    limit: Option<u32>,
) -> Result<Response, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if info.sender != cfg.admin {
        return Err(ContractError::Unauthorized {});
    }
    let limit = limit.unwrap_or(200).min(1000) as usize;

    // Keys are ordered day-first, so the oldest sit at the front and
    // take_while stops the moment we reach the cutoff.
    let stale: Vec<(u64, Addr, String)> = RATE_LIMIT
        .keys(deps.storage, None, None, Order::Ascending)
        .take(limit)
        .collect::<StdResult<Vec<_>>>()?
        .into_iter()
        .take_while(|(d, _, _)| *d < before_day)
        .collect();

    let mut removed = stale.len();
    for (d, addr, ref_id) in stale {
        RATE_LIMIT.remove(deps.storage, (d, &addr, ref_id.as_str()));
    }

    let stale_tiers: Vec<(u64, String, Addr)> = TIER_COUNT
        .keys(deps.storage, None, None, Order::Ascending)
        .take(limit)
        .collect::<StdResult<Vec<_>>>()?
        .into_iter()
        .take_while(|(d, _, _)| *d < before_day)
        .collect();

    removed += stale_tiers.len();
    for (d, action, addr) in stale_tiers {
        TIER_COUNT.remove(deps.storage, (d, action.as_str(), &addr));
    }

    Ok(Response::new()
        .add_attribute("action", "prune_rate_limit")
        .add_attribute("removed", removed.to_string()))
}

fn exec_set_action(
    deps: DepsMut,
    info: MessageInfo,
    item: ActionItem,
) -> Result<Response, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if info.sender != cfg.admin {
        return Err(ContractError::Unauthorized {});
    }
    validate_params(&item.params)?;
    ACTIONS.save(deps.storage, item.key.as_str(), &item.params)?;
    Ok(Response::new()
        .add_attribute("action", "set_action")
        .add_attribute("key", item.key))
}

#[allow(clippy::too_many_arguments)]
fn exec_update_config(
    deps: DepsMut,
    info: MessageInfo,
    attestor: Option<String>,
    treasury: Option<String>,
    pool: Option<String>,
    max_delta: Option<Uint128>,
    half_life_days: Option<u64>,
) -> Result<Response, ContractError> {
    let mut cfg = CONFIG.load(deps.storage)?;
    if info.sender != cfg.admin {
        return Err(ContractError::Unauthorized {});
    }
    if let Some(a) = attestor {
        cfg.attestor = deps.api.addr_validate(&a)?;
    }
    if let Some(a) = treasury {
        cfg.treasury = deps.api.addr_validate(&a)?;
    }
    if let Some(a) = pool {
        cfg.pool = deps.api.addr_validate(&a)?;
    }
    if let Some(m) = max_delta {
        cfg.max_delta = m;
    }
    if let Some(d) = half_life_days {
        if d == 0 {
            return Err(ContractError::InvalidHalfLife {});
        }
        // Stored balances were last decayed under the previous rate; the new
        // rate applies to the whole span since each account's last_update.
        cfg.half_life_secs = d * DAY;
    }
    CONFIG.save(deps.storage, &cfg)?;
    Ok(Response::new().add_attribute("action", "update_config"))
}

fn exec_set_admin(
    deps: DepsMut,
    info: MessageInfo,
    admin: String,
) -> Result<Response, ContractError> {
    let mut cfg = CONFIG.load(deps.storage)?;
    if info.sender != cfg.admin {
        return Err(ContractError::Unauthorized {});
    }
    cfg.admin = deps.api.addr_validate(&admin)?;
    CONFIG.save(deps.storage, &cfg)?;
    Ok(Response::new()
        .add_attribute("action", "set_admin")
        .add_attribute("admin", admin))
}

fn exec_set_paused(
    deps: DepsMut,
    info: MessageInfo,
    paused: bool,
) -> Result<Response, ContractError> {
    let mut cfg = CONFIG.load(deps.storage)?;
    if info.sender != cfg.admin {
        return Err(ContractError::Unauthorized {});
    }
    cfg.paused = paused;
    CONFIG.save(deps.storage, &cfg)?;
    Ok(Response::new()
        .add_attribute("action", "set_paused")
        .add_attribute("paused", paused.to_string()))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_json_binary(&q_config(deps, env)?),
        QueryMsg::Score { address } => to_json_binary(&q_score(deps, env, address)?),
        QueryMsg::EpochScore { address, epoch } => {
            to_json_binary(&q_epoch_score(deps, env, address, epoch)?)
        }
        QueryMsg::Leaderboard {
            epoch,
            start_after,
            limit,
        } => to_json_binary(&q_leaderboard(deps, env, epoch, start_after, limit)?),
        QueryMsg::TierCount { action, address } => {
            to_json_binary(&q_tier_count(deps, env, action, address)?)
        }
    }
}

fn q_config(deps: Deps, env: Env) -> StdResult<ConfigResponse> {
    let cfg = CONFIG.load(deps.storage)?;
    Ok(ConfigResponse {
        admin: cfg.admin.to_string(),
        attestor: cfg.attestor.to_string(),
        treasury: cfg.treasury.to_string(),
        pool: cfg.pool.to_string(),
        half_life_secs: cfg.half_life_secs,
        epoch_len_secs: cfg.epoch_len_secs,
        current_epoch: epoch_of(&cfg, env.block.time.seconds()),
        current_day: env.block.time.seconds() / DAY,
        seeding: cfg.seeding,
        paused: cfg.paused,
    })
}

fn q_score(deps: Deps, env: Env, address: String) -> StdResult<ScoreResponse> {
    let cfg = CONFIG.load(deps.storage)?;
    let addr = deps.api.addr_validate(&address)?;
    let now = env.block.time.seconds();

    match SCORES.may_load(deps.storage, &addr)? {
        Some(e) => {
            let dt = now.saturating_sub(e.last_update);
            Ok(ScoreResponse {
                effective: decay(e.raw, dt, cfg.half_life_secs),
                lifetime_earned: e.lifetime_earned,
                actions: e.actions,
                last_update: e.last_update,
            })
        }
        None => Ok(ScoreResponse {
            effective: Uint128::zero(),
            lifetime_earned: Uint128::zero(),
            actions: 0,
            last_update: 0,
        }),
    }
}

fn q_epoch_score(
    deps: Deps,
    env: Env,
    address: String,
    epoch: Option<u64>,
) -> StdResult<EpochScoreResponse> {
    let cfg = CONFIG.load(deps.storage)?;
    let addr = deps.api.addr_validate(&address)?;
    let ep = epoch.unwrap_or_else(|| epoch_of(&cfg, env.block.time.seconds()));
    let score = EPOCH_SCORES
        .may_load(deps.storage, (ep, &addr))?
        .unwrap_or_default();
    Ok(EpochScoreResponse { epoch: ep, score })
}

fn q_tier_count(
    deps: Deps,
    env: Env,
    action: String,
    address: String,
) -> StdResult<TierCountResponse> {
    let addr = deps.api.addr_validate(&address)?;
    let day = env.block.time.seconds() / DAY;
    let count = TIER_COUNT
        .may_load(deps.storage, (day, action.as_str(), &addr))?
        .unwrap_or(0);
    let next_weight = match ACTIONS.may_load(deps.storage, action.as_str())? {
        Some(p) => resolve_weight(&p, count),
        None => Uint128::zero(),
    };
    Ok(TierCountResponse { count, next_weight })
}

fn q_leaderboard(
    deps: Deps,
    env: Env,
    epoch: Option<u64>,
    start_after: Option<String>,
    limit: Option<u32>,
) -> StdResult<LeaderboardResponse> {
    let cfg = CONFIG.load(deps.storage)?;
    let ep = epoch.unwrap_or_else(|| epoch_of(&cfg, env.block.time.seconds()));
    let limit = limit.unwrap_or(50).min(200) as usize;

    // Bound borrows the address, so it has to outlive the range() call below.
    let start_addr = match start_after {
        Some(s) => Some(deps.api.addr_validate(&s)?),
        None => None,
    };
    let start = start_addr.as_ref().map(|a| Bound::exclusive(a));

    let entries = EPOCH_SCORES
        .prefix(ep)
        .range(deps.storage, start, None, Order::Ascending)
        .take(limit)
        .map(|item| {
            item.map(|(addr, score)| LeaderboardEntry {
                address: addr.to_string(),
                score,
            })
        })
        .collect::<StdResult<Vec<_>>>()?;

    Ok(LeaderboardResponse { epoch: ep, entries })
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, _env: Env, _msg: MigrateMsg) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    Ok(Response::new().add_attribute("action", "migrate"))
}
