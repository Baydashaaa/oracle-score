use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::Uint128;

use crate::state::ActionParams;

#[cw_serde]
pub struct ActionItem {
    pub key: String,
    pub params: ActionParams,
}

#[cw_serde]
pub struct SeedEntry {
    pub user: String,
    pub raw: Uint128,
}

#[cw_serde]
pub struct InstantiateMsg {
    /// Defaults to the instantiating address when omitted.
    pub admin: Option<String>,
    pub attestor: String,
    pub treasury: String,
    /// Second payment recipient, e.g. the weekly draw pool.
    pub pool: String,
    pub half_life_days: u64,
    pub epoch_len_days: u64,
    /// Upper bound on a single attestor-recorded grant.
    pub max_delta: Uint128,
    pub actions: Vec<ActionItem>,
}

#[cw_serde]
pub enum ExecuteMsg {
    /// Paid action. The user pays, the contract grants score and splits the
    /// payment between pool and treasury in the same transaction.
    PaidAction { action: String, ref_id: String },
    /// Free action attested off-chain. Rejected for any action with a price.
    RecordAction {
        user: String,
        action: String,
        ref_id: String,
    },
    /// Import legacy balances. Only valid while seeding is open.
    SeedScores { entries: Vec<SeedEntry> },
    /// Close seeding permanently. Irreversible by design.
    FinalizeSeeding {},
    Slash {
        user: String,
        amount: Uint128,
        reason: String,
    },
    SetAction { item: ActionItem },
    /// Adjust operational parameters without a migration. Epoch length is
    /// deliberately absent: changing it would reindex historical buckets.
    UpdateConfig {
        attestor: Option<String>,
        treasury: Option<String>,
        pool: Option<String>,
        max_delta: Option<Uint128>,
        half_life_days: Option<u64>,
    },
    SetAdmin { admin: String },
    SetPaused { paused: bool },
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    #[returns(ConfigResponse)]
    Config {},
    #[returns(ScoreResponse)]
    Score { address: String },
    #[returns(EpochScoreResponse)]
    EpochScore {
        address: String,
        epoch: Option<u64>,
    },
    /// Paginated by address, not ranked by score. Callers sort client-side.
    #[returns(LeaderboardResponse)]
    Leaderboard {
        epoch: Option<u64>,
        start_after: Option<String>,
        limit: Option<u32>,
    },
    /// How many times an action has been recorded against a ref_id.
    /// Lets the frontend show the next available weight before acting.
    #[returns(RefCountResponse)]
    RefCount { action: String, ref_id: String },
}

#[cw_serde]
pub struct ConfigResponse {
    pub admin: String,
    pub attestor: String,
    pub treasury: String,
    pub pool: String,
    pub half_life_secs: u64,
    pub epoch_len_secs: u64,
    pub current_epoch: u64,
    pub seeding: bool,
    pub paused: bool,
}

#[cw_serde]
pub struct ScoreResponse {
    /// Score with decay applied as of the current block.
    pub effective: Uint128,
    pub lifetime_earned: Uint128,
    pub actions: u64,
    pub last_update: u64,
}

#[cw_serde]
pub struct EpochScoreResponse {
    pub epoch: u64,
    pub score: Uint128,
}

#[cw_serde]
pub struct RefCountResponse {
    pub count: u64,
    /// Weight the next occurrence would earn.
    pub next_weight: Uint128,
}

#[cw_serde]
pub struct LeaderboardEntry {
    pub address: String,
    pub score: Uint128,
}

#[cw_serde]
pub struct LeaderboardResponse {
    pub epoch: u64,
    pub entries: Vec<LeaderboardEntry>,
}

#[cw_serde]
pub struct MigrateMsg {}
