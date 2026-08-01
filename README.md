# Oracle Score

On-chain reputation for [Terra Oracle Classic](https://terraoracle.io), running as a
CosmWasm contract on Terra Classic.

Reputation used to live in a Cloudflare KV namespace and was computed by a Worker.
Anyone using it had to trust that the server was doing what the docs said. Now the
accrual rules, the degressive ladder, the daily limits and the payment split are
executed by the chain, and anyone can verify them.

## Deployments

| Network | Chain ID | Address |
|---|---|---|
| Mainnet | `columbus-5` | `terra1pj6t6v4czktz7znzq8xk2ny2yh7pdwen4jw8z4zz86zrac6ur9vqqkwcls` |
| Testnet | `rebel-2` | `terra1vc92p8nawzfl4q87rhpxazm22d6fjrt884ajt9k95ypdfwg9l2aq9fkwlx` |

Mainnet code id `11551`, checksum
`a8cfa71900ecf426fcb7cddbbaafdabaa17aae4c0a73f190db2f52df486cdf96`.

Reputation was seeded from a snapshot whose canonical hash is
`4aed25763e120f627a3403604c66b20145002ea642487a57bc43d9fe42df4520` - 4 wallets,
1,780 REP. The snapshot is reproducible from `GET /rep/all-time-snapshot` on the
Worker plus the public `questions.json` history.

## Two numbers, not one

A single reputation figure cannot serve both purposes, so the contract keeps two.

**`lifetime_earned` - rank.** Never decays. Drives fee discounts and the weekly
reward multiplier. The published rules promise that rank never resets, and taking a
break should not cost someone the status they earned. An inactive user asks no
questions, so their discount costs nothing.

**`effective` - weight.** Decays with a 90-day half-life. Drives draw entries,
treasury vote weight and gated features. Here the question is not what someone did
once but what they are worth to the network now: without decay, a single burst of
activity would buy permanent influence.

Both come back from one `Score` query. The frontend reads `lifetime_earned` for
rank and `effective` for weight.

**`epoch_score`** is a third figure, bucketed per 7-day epoch and never decayed. It
feeds weekly reward shares, where the fair basis is raw contribution during that
week.

## Scale

Score is stored in micro-units, exactly like `uluna`: a weight of 40 is written as
`40000000`. Divide by 1e6 for display, and **round rather than floor** - decay
applied between blocks otherwise turns a freshly granted 40 into a displayed 39.

This is not cosmetic. In whole units, `multiply_ratio` floored roughly a full point
off every accrual, so five granted points showed up as four - a 20% leak that
compounded on every write. Caught on testnet, fixed in v0.2.0.

## Actions

Each action is configured independently:

```jsonc
{
  "weight": "40000000",       // score granted, in micro-units
  "tiers": [                  // optional descending ladder, per user per UTC day
    { "up_to": 3,  "weight": "40000000" },
    { "up_to": 10, "weight": "10000000" }
  ],
  "price": "150000000000",    // uluna required; 0 means free (attestor only)
  "pool_amount": "100000000000", // fixed uluna routed to the pool address
  "daily_limit": 3            // per user per ref_id per day; 0 = unlimited
}
```

**Tiers count per user per UTC day, across all references.** The ladder exists to
stop one person farming answers, so the fourth answer of someone's day is worth
less regardless of which question it lands on. Counting per question instead would
punish whoever answers a popular thread late - an earlier version did exactly that,
which is why the semantics are spelled out here.

**`pool_amount` is fixed, not a percentage.** Rank and streak discounts reduce only
the treasury leg; the pool leg backs draw entries at a protocol-wide rate of 25,000
LUNC per entry and must stay exact whoever pays. `price` is therefore set to the
*maximum-discount floor* - 150,000 for Priority, 37,500 for Basic - and anyone
paying full price simply sends more to the treasury. Overpayment lands entirely on
the treasury leg too.

The contract never holds funds: `PaidAction` forwards everything in the same
transaction, and a test asserts the contract balance stays zero.

## Messages

**Execute**

| Message | Who | Notes |
|---|---|---|
| `PaidAction { action, ref_id }` | anyone | with `uluna` attached |
| `RecordAction { user, action, ref_id }` | attestor | free actions only |
| `SeedScores { entries }` | admin | only while seeding is open |
| `FinalizeSeeding {}` | admin | **irreversible** |
| `Slash { user, amount, reason }` | admin | cuts weight *and* rank |
| `PruneRateLimit { before_day, limit }` | admin | paginated; clears both day-keyed maps |
| `SetAction { item }` | admin | validates `pool_amount <= price` |
| `UpdateConfig { … }` | admin | attestor, treasury, pool, max_delta, half_life_days |
| `SetAdmin { admin }` / `SetPaused { paused }` | admin | |

**Query**

`Config {}` · `Score { address }` · `EpochScore { address, epoch? }` ·
`Leaderboard { epoch?, start_after?, limit? }` · `TierCount { action, address }`

`Leaderboard` paginates by address, not by score - on-chain sorting by value needs
a secondary index. Callers fetch pages and sort client-side.

## Guarantees worth knowing

- The attestor can only record actions whose `price` is zero. A compromised
  attestor key can grant free-action score, but cannot mint score for paid actions,
  change parameters or migrate the contract.
- `max_delta` caps any single attestor grant.
- Seeding closes permanently. After `FinalizeSeeding`, not even the admin can
  rewrite balances.
- Overflow checks stay on in the release profile. Do not disable them.

## Build

Reproducibility is the point: anyone should be able to rebuild and match the
checksum on chain.

```bash
cargo test

docker run --rm -v "$(pwd)":/code \
  --mount type=volume,source="$(basename "$(pwd)")_cache",target=/target \
  --mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
  cosmwasm/optimizer:0.17.0

cat artifacts/checksums.txt
```

Compare against the chain:

```bash
curl -s https://terra-classic-lcd.publicnode.com/cosmwasm/wasm/v1/code/11551
```

Building locally with a modern toolchain instead of the optimizer produces a
binary the CosmWasm 1.x VM may reject - Rust 1.82+ emits wasm extensions the VM
does not accept. Use the optimizer image; it pins the right compiler.

## Updating the contract

GitHub does not deploy anything here. Pushing publishes the source; only a signed
transaction changes what runs on chain.

1. Edit, then `cargo test`
2. Bump the version in `Cargo.toml`, run `cargo schema`
3. Build with the optimizer
4. `git push` - for history and verification
5. `./migrate.sh` on rebel-2, verify the behaviour
6. `./migrate-mainnet.sh`

The contract address never changes. Scores, epochs and counters survive migration.

**The one real trap:** changing a struct that is already in storage makes existing
records unreadable. Replacing `pool_bps` with `pool_amount` meant every configured
action had to be re-applied with `SetAction` after the migration. Three ways out:

- make new fields `Option<T>` so old records still deserialize
- re-apply the config after migrating, as above
- write real conversion logic in `migrate()`

`Config` and `ScoreEntry` deserve the most care - they hold live user data and
cannot simply be re-applied.

Always migrate rebel-2 first. It costs nothing and has already caught three bugs
that would have reached mainnet: the rounding leak, the tier scope, and discounts
breaking a percentage-based split.

## Off-chain pieces

**Streak multipliers stay in KV.** The frontend applies them on top of the
contract's base. Porting grace days and milestones on-chain buys nothing.

**Free actions are queued, not yet recorded.** The Worker writes `pending-rep:`
records for answers and answer upvotes from the moment the queue shipped, so
nothing is lost. A scheduled job will drain them into batched `RecordAction`
calls, with the attestor key in CI secrets rather than in the Worker. Until that
job exists, free-action reputation is off-chain and should be described that way.

## Keys

| Key | Role |
|---|---|
| `oracle-admin` | contract admin - migrations, parameters, slashing |
| `oracle-attestor` | records free actions; holds gas only |

The admin mnemonic is the only irrecoverable secret in the project: lose it and the
contract is frozen at its current version forever. Paper backup, never in CI, never
in a file.
