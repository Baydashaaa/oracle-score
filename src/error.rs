use cosmwasm_std::{StdError, Uint128};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("Unauthorized")]
    Unauthorized {},

    #[error("Contract is paused")]
    Paused {},

    #[error("Seeding is already closed")]
    SeedingClosed {},

    #[error("Unknown action: {action}")]
    UnknownAction { action: String },

    #[error("Action is not payable")]
    NotPayable {},

    #[error("Action is not free — must be paid on-chain")]
    NotFree {},

    #[error("Daily limit reached for this reference")]
    RateLimited {},

    #[error("Delta exceeds max_delta")]
    DeltaTooLarge {},

    #[error("Insufficient payment: {need} uluna required")]
    InsufficientPayment { need: Uint128 },

    #[error("Only uluna is accepted")]
    UnexpectedDenom {},

    #[error("pool_bps must not exceed 10000")]
    InvalidBps {},

    #[error("half_life_days must be greater than zero")]
    InvalidHalfLife {},
}
