[README.md](https://github.com/user-attachments/files/30621357/README.md)
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

Mainnet code id `11571`, checksum
`af9ad9eb7ad46700d29c56da97b42da27b4085c270113cfd3a5bf21b6f72af58`.

Reputation was seeded from a snapshot whose canonical hash is
`e02fd7a59c38060ca6924856c9f01b9af7acefd59e94e96b3e721cbd01392f2c` - 4 wallets,
1,890 REP, written on 2026-08-01 by
[`796D21C349D02A3F5A9A22ECB831B03F5A54FD33E46D4FB0F533EE393A327741`](https://finder.terraport.finance/mainnet/tx/796D21C349D02A3F5A9A22ECB831B03F5A54FD33E46D4FB0F533EE393A327741).

An earlier seeding on 2026-07-31 wrote 1,780 REP; `SeedScores` replaces an entry
rather than adding to it, so the second call superseded the first. Seeding was
then closed permanently by `FinalizeSeeding` on 2026-08-03.

Verify it against the chain, not against the Worker: the seeding transaction
above is public and immutable. The `GET /rep/all-time-snapshot` endpoint
recomputes reputation **as of the moment you call it**, so it cannot reproduce a
past snapshot - today it returns a different wallet count and total.

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
curl -s https://terra-classic-lcd.publicnode.com/cosmwasm/wasm/v1/code/11571
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

## Re-seeding

`SeedScores` overwrites rather than adds, so re-seeding from a fresh snapshot is
safe in itself — balances simply become what the snapshot says. What is not safe
is doing it alone.

The attestor skips any queued record older than `SNAPSHOT_TS`, because anything
before that moment is already inside the seeded figure. Re-seed with a newer
snapshot and leave that constant behind, and the next hourly run will record the
actions between the two snapshots **on top of** the value that already includes
them. Everyone in the queue gets paid twice.

So the order is fixed:

1. Let the queue drain (`GET /rep/pending` comes back empty)
2. Take the snapshot, keep its `generatedAt`
3. Raise `SNAPSHOT_TS` in `scripts/oracle-score-attest.js` to that timestamp and push
4. Only then `SeedScores`
5. Verify the four balances, publish the new snapshot hash

Step 3 before step 4, always. The reverse order is silent — nothing errors, the
numbers are just wrong a hour later.

Re-seeding is only possible while seeding is open. `FinalizeSeeding` closes it
permanently, which is why it waits until the frontend reads from the contract and
people have confirmed their own ranks.

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
