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
const BPS_DENOMINATOR: u128 = 10_000;

fn validate_params(p: &ActionParams) -> Result<(), ContractError> {
    if p.pool_bps as u128 > BPS_DENOMINATOR {
        return Err(ContractError::InvalidBps {});
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
        } => exec_record_action(deps, env, info, user, action, ref_id),
        ExecuteMsg::SeedScores { entries } => exec_seed(deps, env, info, entries),
        ExecuteMsg::FinalizeSeeding {} => exec_finalize_seeding(deps, info),
        ExecuteMsg::Slash {
            user,
            amount,
            reason,
        } => exec_slash(deps, env, info, user, amount, reason),
        ExecuteMsg::SetAction { item } => exec_set_action(deps, info, item),
        ExecuteMsg::SetAttestor { attestor } => exec_set_attestor(deps, info, attestor),
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
    let key = (user, ref_id, day);
    let used = RATE_LIMIT.may_load(storage, key)?.unwrap_or(0);
    if used >= limit {
        return Err(ContractError::RateLimited {});
    }
    RATE_LIMIT.save(storage, key, &(used + 1))?;
    Ok(())
}

/// Returns the count of prior occurrences, then records this one.
fn bump_ref_count(
    storage: &mut dyn Storage,
    action: &str,
    ref_id: &str,
) -> StdResult<u64> {
    let key = (action, ref_id);
    let prior = REF_COUNT.may_load(storage, key)?.unwrap_or(0);
    REF_COUNT.save(storage, key, &(prior + 1))?;
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
    let prior = bump_ref_count(deps.storage, &action, &ref_id)?;
    let weight = resolve_weight(&params, prior);
    let new_raw = accrue(deps.storage, &cfg, now, &info.sender, weight)?;

    // pool_bps is validated at or below 10000, so the pool share never
    // exceeds the payment and the subtraction below cannot underflow.
    let to_pool = paid.multiply_ratio(params.pool_bps as u128, BPS_DENOMINATOR);
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

fn exec_record_action(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    user: String,
    action: String,
    ref_id: String,
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

    // The attestor must never be able to grant score for paid actions.
    if !params.price.is_zero() {
        return Err(ContractError::NotFree {});
    }

    let user_addr = deps.api.addr_validate(&user)?;
    let now = env.block.time.seconds();
    check_rate_limit(deps.storage, now, &user_addr, &ref_id, params.daily_limit)?;
    let prior = bump_ref_count(deps.storage, &action, &ref_id)?;
    let weight = resolve_weight(&params, prior);

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
    e.last_update = now;
    SCORES.save(deps.storage, &addr, &e)?;

    Ok(Response::new()
        .add_attribute("action", "slash")
        .add_attribute("user", user)
        .add_attribute("amount", amount.to_string())
        .add_attribute("reason", reason)
        .add_attribute("raw", e.raw.to_string()))
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

fn exec_set_attestor(
    deps: DepsMut,
    info: MessageInfo,
    attestor: String,
) -> Result<Response, ContractError> {
    let mut cfg = CONFIG.load(deps.storage)?;
    if info.sender != cfg.admin {
        return Err(ContractError::Unauthorized {});
    }
    cfg.attestor = deps.api.addr_validate(&attestor)?;
    CONFIG.save(deps.storage, &cfg)?;
    Ok(Response::new()
        .add_attribute("action", "set_attestor")
        .add_attribute("attestor", attestor))
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
        QueryMsg::RefCount { action, ref_id } => {
            to_json_binary(&q_ref_count(deps, action, ref_id)?)
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

fn q_ref_count(deps: Deps, action: String, ref_id: String) -> StdResult<RefCountResponse> {
    let count = REF_COUNT
        .may_load(deps.storage, (action.as_str(), ref_id.as_str()))?
        .unwrap_or(0);
    let next_weight = match ACTIONS.may_load(deps.storage, action.as_str())? {
        Some(p) => resolve_weight(&p, count),
        None => Uint128::zero(),
    };
    Ok(RefCountResponse { count, next_weight })
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
