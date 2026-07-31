use cosmwasm_std::{coin, coins, Addr, Empty, Uint128};
use cw_multi_test::{App, AppBuilder, Contract, ContractWrapper, Executor};

use oracle_score::contract::{execute, instantiate, migrate, query};
use oracle_score::msg::*;
use oracle_score::state::{ActionParams, WeightTier};

const ADMIN: &str = "admin";
const ATTESTOR: &str = "attestor";
const TREASURY: &str = "treasury";
const POOL: &str = "pool";
const USER: &str = "user0001";
const OTHER: &str = "user0002";
const DENOM: &str = "uluna";
const FOREIGN: &str = "uusd";

const QUESTION_PRICE: u128 = 200_000;
const CHAT_PRICE: u128 = 5_000;

fn contract() -> Box<dyn Contract<Empty>> {
    Box::new(ContractWrapper::new(execute, instantiate, query).with_migrate(migrate))
}

fn params(
    weight: u128,
    tiers: Vec<(u64, u128)>,
    price: u128,
    pool_bps: u16,
    daily_limit: u8,
) -> ActionParams {
    ActionParams {
        weight: Uint128::new(weight),
        tiers: tiers
            .into_iter()
            .map(|(up_to, w)| WeightTier {
                up_to,
                weight: Uint128::new(w),
            })
            .collect(),
        price: Uint128::new(price),
        pool_bps,
        daily_limit,
    }
}

fn setup() -> (App, Addr) {
    let mut app = AppBuilder::new().build(|router, _, storage| {
        for who in [USER, OTHER] {
            router
                .bank
                .init_balance(
                    storage,
                    &Addr::unchecked(who),
                    vec![coin(10_000_000, DENOM), coin(10_000_000, FOREIGN)],
                )
                .unwrap();
        }
    });

    let code_id = app.store_code(contract());
    let addr = app
        .instantiate_contract(
            code_id,
            Addr::unchecked(ADMIN),
            &InstantiateMsg {
                admin: None,
                attestor: ATTESTOR.to_string(),
                treasury: TREASURY.to_string(),
                pool: POOL.to_string(),
                half_life_days: 90,
                epoch_len_days: 7,
                max_delta: Uint128::new(100),
                actions: vec![
                    // Paid, split evenly between the weekly pool and treasury.
                    ActionItem {
                        key: "question".to_string(),
                        params: params(40, vec![], QUESTION_PRICE, 5_000, 0),
                    },
                    // Paid, entirely to treasury.
                    ActionItem {
                        key: "chat".to_string(),
                        params: params(5, vec![], CHAT_PRICE, 0, 0),
                    },
                    // Free, degressive: answers 1-3 earn 40, 4-10 earn 10, then nothing.
                    ActionItem {
                        key: "answer".to_string(),
                        params: params(0, vec![(3, 40), (10, 10)], 0, 0, 3),
                    },
                ],
            },
            &[],
            "oracle-score",
            Some(ADMIN.to_string()),
        )
        .unwrap();

    (app, addr)
}

fn score_of(app: &App, contract: &Addr, who: &str) -> Uint128 {
    let r: ScoreResponse = app
        .wrap()
        .query_wasm_smart(
            contract,
            &QueryMsg::Score {
                address: who.to_string(),
            },
        )
        .unwrap();
    r.effective
}

fn balance(app: &App, who: &str) -> Uint128 {
    app.wrap().query_balance(who, DENOM).unwrap().amount
}

fn pay(
    app: &mut App,
    c: &Addr,
    who: &str,
    action: &str,
    ref_id: &str,
    funds: &[cosmwasm_std::Coin],
) -> anyhow::Result<()> {
    app.execute_contract(
        Addr::unchecked(who),
        c.clone(),
        &ExecuteMsg::PaidAction {
            action: action.to_string(),
            ref_id: ref_id.to_string(),
        },
        funds,
    )
    .map(|_| ())
    .map_err(|e| anyhow::anyhow!(e.root_cause().to_string()))
}

fn ask(app: &mut App, c: &Addr, who: &str, ref_id: &str) -> anyhow::Result<()> {
    pay(app, c, who, "question", ref_id, &coins(QUESTION_PRICE, DENOM))
}

fn answer(app: &mut App, c: &Addr, user: &str, ref_id: &str) -> anyhow::Result<()> {
    app.execute_contract(
        Addr::unchecked(ATTESTOR),
        c.clone(),
        &ExecuteMsg::RecordAction {
            user: user.to_string(),
            action: "answer".to_string(),
            ref_id: ref_id.to_string(),
        },
        &[],
    )
    .map(|_| ())
    .map_err(|e| anyhow::anyhow!(e.root_cause().to_string()))
}

#[test]
fn payment_splits_between_pool_and_treasury() {
    let (mut app, c) = setup();

    ask(&mut app, &c, USER, "q1").unwrap();

    assert_eq!(score_of(&app, &c, USER), Uint128::new(40));
    assert_eq!(balance(&app, POOL), Uint128::new(QUESTION_PRICE / 2));
    assert_eq!(balance(&app, TREASURY), Uint128::new(QUESTION_PRICE / 2));
    // Nothing may linger in the contract.
    assert_eq!(balance(&app, c.as_str()), Uint128::zero());
}

#[test]
fn overpayment_is_split_on_the_full_amount() {
    let (mut app, c) = setup();

    pay(
        &mut app,
        &c,
        USER,
        "question",
        "q1",
        &coins(QUESTION_PRICE * 2, DENOM),
    )
    .unwrap();

    assert_eq!(balance(&app, POOL), Uint128::new(QUESTION_PRICE));
    assert_eq!(balance(&app, TREASURY), Uint128::new(QUESTION_PRICE));
    assert_eq!(balance(&app, c.as_str()), Uint128::zero());
}

#[test]
fn zero_pool_bps_sends_everything_to_treasury() {
    let (mut app, c) = setup();

    pay(&mut app, &c, USER, "chat", "m1", &coins(CHAT_PRICE, DENOM)).unwrap();

    assert_eq!(balance(&app, POOL), Uint128::zero());
    assert_eq!(balance(&app, TREASURY), Uint128::new(CHAT_PRICE));
    assert_eq!(score_of(&app, &c, USER), Uint128::new(5));
}

#[test]
fn foreign_denom_is_rejected() {
    let (mut app, c) = setup();

    let err = pay(
        &mut app,
        &c,
        USER,
        "question",
        "q1",
        &coins(QUESTION_PRICE, FOREIGN),
    )
    .unwrap_err();

    assert!(err.to_string().contains("uluna"));
    assert_eq!(score_of(&app, &c, USER), Uint128::zero());
}

#[test]
fn underpayment_is_rejected() {
    let (mut app, c) = setup();

    let err = pay(
        &mut app,
        &c,
        USER,
        "question",
        "q1",
        &coins(QUESTION_PRICE - 1, DENOM),
    )
    .unwrap_err();

    assert!(err.to_string().contains("Insufficient payment"));
    assert_eq!(score_of(&app, &c, USER), Uint128::zero());
    assert_eq!(balance(&app, TREASURY), Uint128::zero());
}

#[test]
fn answer_weight_steps_down_across_users_on_the_same_question() {
    let (mut app, c) = setup();

    // The counter is per question, not per user, so the fourth answer
    // is worth less even though a different person wrote it.
    for _ in 0..3 {
        answer(&mut app, &c, USER, "q1").unwrap();
    }
    for _ in 0..3 {
        answer(&mut app, &c, OTHER, "q1").unwrap();
    }

    assert_eq!(score_of(&app, &c, USER), Uint128::new(120));
    assert_eq!(score_of(&app, &c, OTHER), Uint128::new(30));
}

#[test]
fn daily_limit_blocks_the_fourth_answer_from_one_user() {
    let (mut app, c) = setup();

    for _ in 0..3 {
        answer(&mut app, &c, USER, "q1").unwrap();
    }
    let err = answer(&mut app, &c, USER, "q1").unwrap_err();
    assert!(err.to_string().contains("Daily limit"));

    // A different question is unaffected and starts at the top tier.
    answer(&mut app, &c, USER, "q2").unwrap();
    assert_eq!(score_of(&app, &c, USER), Uint128::new(160));

    // Next day the limit lifts, but q1 has moved down a tier.
    // The existing 160 ages one day first: 160 * 15465600/15552000 = 159, then +10.
    app.update_block(|b| b.time = b.time.plus_seconds(86_400));
    answer(&mut app, &c, USER, "q1").unwrap();
    assert_eq!(score_of(&app, &c, USER), Uint128::new(169));
}

#[test]
fn ref_count_query_reports_the_next_weight() {
    let (mut app, c) = setup();

    let fresh: RefCountResponse = app
        .wrap()
        .query_wasm_smart(
            &c,
            &QueryMsg::RefCount {
                action: "answer".to_string(),
                ref_id: "q1".to_string(),
            },
        )
        .unwrap();
    assert_eq!(fresh.count, 0);
    assert_eq!(fresh.next_weight, Uint128::new(40));

    for _ in 0..3 {
        answer(&mut app, &c, USER, "q1").unwrap();
    }

    let after: RefCountResponse = app
        .wrap()
        .query_wasm_smart(
            &c,
            &QueryMsg::RefCount {
                action: "answer".to_string(),
                ref_id: "q1".to_string(),
            },
        )
        .unwrap();
    assert_eq!(after.count, 3);
    assert_eq!(after.next_weight, Uint128::new(10));
}

#[test]
fn attestor_cannot_grant_score_for_paid_actions() {
    let (mut app, c) = setup();

    let err = app
        .execute_contract(
            Addr::unchecked(ATTESTOR),
            c.clone(),
            &ExecuteMsg::RecordAction {
                user: USER.to_string(),
                action: "question".to_string(),
                ref_id: "q1".to_string(),
            },
            &[],
        )
        .unwrap_err();

    assert!(err.root_cause().to_string().contains("not free"));
    assert_eq!(score_of(&app, &c, USER), Uint128::zero());
}

#[test]
fn random_wallet_cannot_record_actions() {
    let (mut app, c) = setup();

    let err = app
        .execute_contract(
            Addr::unchecked(OTHER),
            c.clone(),
            &ExecuteMsg::RecordAction {
                user: OTHER.to_string(),
                action: "answer".to_string(),
                ref_id: "q1".to_string(),
            },
            &[],
        )
        .unwrap_err();

    assert!(err.root_cause().to_string().contains("Unauthorized"));
    assert_eq!(score_of(&app, &c, OTHER), Uint128::zero());
}

#[test]
fn invalid_bps_is_rejected() {
    let (mut app, c) = setup();

    let err = app
        .execute_contract(
            Addr::unchecked(ADMIN),
            c.clone(),
            &ExecuteMsg::SetAction {
                item: ActionItem {
                    key: "question".to_string(),
                    params: params(40, vec![], QUESTION_PRICE, 10_001, 0),
                },
            },
            &[],
        )
        .unwrap_err();

    assert!(err.root_cause().to_string().contains("pool_bps"));
}

#[test]
fn seeding_closes_permanently() {
    let (mut app, c) = setup();

    app.execute_contract(
        Addr::unchecked(ADMIN),
        c.clone(),
        &ExecuteMsg::SeedScores {
            entries: vec![SeedEntry {
                user: USER.to_string(),
                raw: Uint128::new(5000),
            }],
        },
        &[],
    )
    .unwrap();
    assert_eq!(score_of(&app, &c, USER), Uint128::new(5000));

    app.execute_contract(
        Addr::unchecked(ADMIN),
        c.clone(),
        &ExecuteMsg::FinalizeSeeding {},
        &[],
    )
    .unwrap();

    // Even the admin cannot reopen it.
    let err = app
        .execute_contract(
            Addr::unchecked(ADMIN),
            c.clone(),
            &ExecuteMsg::SeedScores {
                entries: vec![SeedEntry {
                    user: OTHER.to_string(),
                    raw: Uint128::new(999_999),
                }],
            },
            &[],
        )
        .unwrap_err();

    assert!(err.root_cause().to_string().contains("closed"));
    assert_eq!(score_of(&app, &c, OTHER), Uint128::zero());
}

#[test]
fn non_admin_cannot_seed() {
    let (mut app, c) = setup();

    let err = app
        .execute_contract(
            Addr::unchecked(OTHER),
            c.clone(),
            &ExecuteMsg::SeedScores {
                entries: vec![SeedEntry {
                    user: OTHER.to_string(),
                    raw: Uint128::new(999_999),
                }],
            },
            &[],
        )
        .unwrap_err();

    assert!(err.root_cause().to_string().contains("Unauthorized"));
}

#[test]
fn score_decays_between_queries() {
    let (mut app, c) = setup();

    ask(&mut app, &c, USER, "q1").unwrap();
    assert_eq!(score_of(&app, &c, USER), Uint128::new(40));

    app.update_block(|b| b.time = b.time.plus_seconds(90 * 86_400));
    assert_eq!(score_of(&app, &c, USER), Uint128::new(20));

    app.update_block(|b| b.time = b.time.plus_seconds(90 * 86_400));
    assert_eq!(score_of(&app, &c, USER), Uint128::new(10));
}

#[test]
fn epoch_score_resets_each_week_while_raw_persists() {
    let (mut app, c) = setup();

    ask(&mut app, &c, USER, "q1").unwrap();

    let this_week: EpochScoreResponse = app
        .wrap()
        .query_wasm_smart(
            &c,
            &QueryMsg::EpochScore {
                address: USER.to_string(),
                epoch: None,
            },
        )
        .unwrap();
    assert_eq!(this_week.epoch, 0);
    assert_eq!(this_week.score, Uint128::new(40));

    app.update_block(|b| b.time = b.time.plus_seconds(7 * 86_400));

    let next_week: EpochScoreResponse = app
        .wrap()
        .query_wasm_smart(
            &c,
            &QueryMsg::EpochScore {
                address: USER.to_string(),
                epoch: None,
            },
        )
        .unwrap();
    assert_eq!(next_week.epoch, 1);
    assert_eq!(next_week.score, Uint128::zero());

    // Epoch buckets are raw sums and never decay, so history stays exact.
    let past: EpochScoreResponse = app
        .wrap()
        .query_wasm_smart(
            &c,
            &QueryMsg::EpochScore {
                address: USER.to_string(),
                epoch: Some(0),
            },
        )
        .unwrap();
    assert_eq!(past.score, Uint128::new(40));
    // The cumulative score does decay: 40 * 14947200/15552000 after one week.
    assert_eq!(score_of(&app, &c, USER), Uint128::new(38));
}

#[test]
fn pause_stops_everything_and_lifts_cleanly() {
    let (mut app, c) = setup();

    app.execute_contract(
        Addr::unchecked(ADMIN),
        c.clone(),
        &ExecuteMsg::SetPaused { paused: true },
        &[],
    )
    .unwrap();

    assert!(ask(&mut app, &c, USER, "q1")
        .unwrap_err()
        .to_string()
        .contains("paused"));
    assert!(answer(&mut app, &c, USER, "q1")
        .unwrap_err()
        .to_string()
        .contains("paused"));

    app.execute_contract(
        Addr::unchecked(ADMIN),
        c.clone(),
        &ExecuteMsg::SetPaused { paused: false },
        &[],
    )
    .unwrap();

    ask(&mut app, &c, USER, "q1").unwrap();
    assert_eq!(score_of(&app, &c, USER), Uint128::new(40));
}

#[test]
fn slash_reduces_score_and_only_admin_can_do_it() {
    let (mut app, c) = setup();

    ask(&mut app, &c, USER, "q1").unwrap();
    ask(&mut app, &c, USER, "q2").unwrap();
    assert_eq!(score_of(&app, &c, USER), Uint128::new(80));

    let err = app
        .execute_contract(
            Addr::unchecked(OTHER),
            c.clone(),
            &ExecuteMsg::Slash {
                user: USER.to_string(),
                amount: Uint128::new(50),
                reason: "nope".to_string(),
            },
            &[],
        )
        .unwrap_err();
    assert!(err.root_cause().to_string().contains("Unauthorized"));

    app.execute_contract(
        Addr::unchecked(ADMIN),
        c.clone(),
        &ExecuteMsg::Slash {
            user: USER.to_string(),
            amount: Uint128::new(50),
            reason: "spam".to_string(),
        },
        &[],
    )
    .unwrap();
    assert_eq!(score_of(&app, &c, USER), Uint128::new(30));

    // Slashing past zero must not underflow.
    app.execute_contract(
        Addr::unchecked(ADMIN),
        c.clone(),
        &ExecuteMsg::Slash {
            user: USER.to_string(),
            amount: Uint128::new(1_000_000),
            reason: "reset".to_string(),
        },
        &[],
    )
    .unwrap();
    assert_eq!(score_of(&app, &c, USER), Uint128::zero());
}

// ---- precision regression, found on rebel-2 ----

const SCALE: u128 = 1_000_000;

fn setup_with(answer_weight: u128, max_delta: u128) -> (App, Addr) {
    let mut app = AppBuilder::new().build(|router, _, storage| {
        router
            .bank
            .init_balance(storage, &Addr::unchecked(USER), coins(10_000_000, DENOM))
            .unwrap();
    });
    let code_id = app.store_code(contract());
    let addr = app
        .instantiate_contract(
            code_id,
            Addr::unchecked(ADMIN),
            &InstantiateMsg {
                admin: None,
                attestor: ATTESTOR.to_string(),
                treasury: TREASURY.to_string(),
                pool: POOL.to_string(),
                half_life_days: 90,
                epoch_len_days: 7,
                max_delta: Uint128::new(max_delta),
                actions: vec![ActionItem {
                    key: "answer".to_string(),
                    params: params(answer_weight, vec![], 0, 0, 0),
                }],
            },
            &[],
            "oracle-score-precision",
            Some(ADMIN.to_string()),
        )
        .unwrap();
    (app, addr)
}

fn answer_ref(app: &mut App, c: &Addr, ref_id: &str) {
    app.execute_contract(
        Addr::unchecked(ATTESTOR),
        c.clone(),
        &ExecuteMsg::RecordAction {
            user: USER.to_string(),
            action: "answer".to_string(),
            ref_id: ref_id.to_string(),
        },
        &[],
    )
    .unwrap();
}

/// Four grants of 40, fifteen seconds apart, exactly like real blocks.
/// Integer division floors away almost a full point on every write.
#[test]
fn unscaled_weights_leak_a_point_per_action() {
    let (mut app, c) = setup_with(40, 100);
    for i in 0..4 {
        answer_ref(&mut app, &c, &format!("q{}", i));
        app.update_block(|b| b.time = b.time.plus_seconds(15));
    }
    let earned = Uint128::new(160);
    let actual = score_of(&app, &c, USER);
    assert!(
        earned - actual >= Uint128::new(4),
        "expected visible loss, got {} of {}",
        actual,
        earned
    );
}

/// The same run in micro-units: the floor costs parts per million.
#[test]
fn scaled_weights_keep_the_loss_negligible() {
    let (mut app, c) = setup_with(40 * SCALE, 100 * SCALE);
    for i in 0..4 {
        answer_ref(&mut app, &c, &format!("q{}", i));
        app.update_block(|b| b.time = b.time.plus_seconds(15));
    }
    let earned = Uint128::new(160 * SCALE);
    let lost = earned - score_of(&app, &c, USER);
    assert!(lost < Uint128::new(1_000), "lost {} of {}", lost, earned);
}

#[test]
fn only_admin_can_update_config() {
    let (mut app, c) = setup_with(40 * SCALE, 100 * SCALE);

    let msg = ExecuteMsg::UpdateConfig {
        attestor: None,
        treasury: None,
        pool: None,
        max_delta: Some(Uint128::new(999 * SCALE)),
        half_life_days: Some(30),
    };

    let err = app
        .execute_contract(Addr::unchecked(OTHER), c.clone(), &msg, &[])
        .unwrap_err();
    assert!(err.root_cause().to_string().contains("Unauthorized"));

    app.execute_contract(Addr::unchecked(ADMIN), c.clone(), &msg, &[])
        .unwrap();

    let cfg: ConfigResponse = app
        .wrap()
        .query_wasm_smart(&c, &QueryMsg::Config {})
        .unwrap();
    assert_eq!(cfg.half_life_secs, 30 * 86_400);
}

#[test]
fn zero_half_life_is_rejected() {
    let (mut app, c) = setup_with(40 * SCALE, 100 * SCALE);
    let err = app
        .execute_contract(
            Addr::unchecked(ADMIN),
            c.clone(),
            &ExecuteMsg::UpdateConfig {
                attestor: None,
                treasury: None,
                pool: None,
                max_delta: None,
                half_life_days: Some(0),
            },
            &[],
        )
        .unwrap_err();
    assert!(err.root_cause().to_string().contains("half_life_days"));
}
