# Proof of Stake: the on-chain program

**Stake a pump.fun coin. Earn 100% of its trading fees, in SOL, every minute.**

This repository is the Solana program behind [Proof of Stake](https://proofofstake.fun): the
code that holds every staked coin and splits every fee. It is public so that
anyone can check, instead of trusting us, that:

- 100% of a coin's creator fees go to the people who stake it, and nobody can change that;
- staked coins can only go back to the wallet that staked them;
- the program deployed on Solana is built from exactly this code.

| | |
|---|---|
| Program | [`PoSBsxAZVhLmPAUv9YLcWHauVjzSro6smzt92JPWtLY`](https://solscan.io/account/PoSBsxAZVhLmPAUv9YLcWHauVjzSro6smzt92JPWtLY) |
| Network | Solana mainnet-beta |
| Framework | Anchor 0.31.1 |
| Verify it yourself | [VERIFY.md](VERIFY.md) |
| Trust model, honestly | [SECURITY.md](SECURITY.md) |

---

## How it works, in four lines

1. A coin launched on Proof of Stake is created on pump.fun with a **program address as its creator**: an address with no private key, owned by this program.
2. That address signs exactly one thing, once: pump.fun's fee-sharing setting, written as **`{ coin's pool: 100% }`**. pump.fun then revokes every right to change it, for everyone, forever.
3. Every trade on pump.fun (and on PumpSwap after graduation) pays a creator fee. pump.fun sends it to the pool.
4. Every minute, the fees that arrived are split **pro rata** between the coins that were staked for the whole minute, and sent to each staker's wallet in SOL.

To leave, a staker unstakes: their coins stop earning at once and can be taken back **one hour** later. The wait is what stops anyone from jumping in just before a payout and out right after.

## What the program guarantees

These are enforced by the code in this repository, not by a promise.

| Guarantee | Where |
|---|---|
| The fee split is exactly 100% to the pool. Any other split, any second shareholder (even at 0%), any other instruction is refused. | `lock_fee_split`, `check_update_data` (fuzz-tested) |
| The coin's creator address signs nothing else, ever. | `lock_fee_split` is the only instruction that uses it |
| Staked coins only go back to the wallet that owns the position. | `unstake`, `request_unstake`, `close_position` |
| SOL is only ever paid to the position's owner. Whoever pushes a payout keeps at most 5,000 lamports for the network fee, and nothing when the owner claims it themselves. | `claim`, `MAX_CLAIM_FEE` |
| A stake earns only from the next full minute; an exit stops earning immediately. | `crates/stake-math` |
| Nobody can be credited more coins than the pool actually received. | `credit_deposit` (`vault_accounted` ≤ vault balance) |
| The platform takes **nothing** from trading fees. Its only revenue is two flat fees, capped in the code: launching a coin (≤ 0.2 SOL) and unstaking (≤ 0.05 SOL). | `MAX_LAUNCH_FEE`, `MAX_EXIT_FEE`, `set_fees` |
| A wallet that stakes by signing never depends on our servers: it can stake, claim, unstake and take its coins back on its own. | `stake`, `claim`, `request_unstake`, `unstake` |

## Instructions

| Instruction | Who can call it | What it does |
|---|---|---|
| `init_pool` | the launcher | Opens the coin's staking pool. Pays the flat launch fee. |
| `lock_fee_split` | anyone | Writes `{pool: 100%}` into pump.fun's fee sharing, signed by the coin's creator address. Refuses anything else. |
| `stake` | the staker (signs) | Moves coins into the pool. Credited at once, earning from the next minute. |
| `credit_deposit` | the attestor | For people who stake by **sending** coins to the pool address instead of connecting a wallet: credits the wallet that sent them (see SECURITY.md). |
| `request_unstake` | the staker (signs) | Starts leaving. Pays the flat exit fee. Coins free after 1 hour. |
| `request_exit` | the attestor | Same, for a staker who asked by sending 0.01 SOL to the pool address. |
| `unstake` | anyone | After the wait, sends the coins back to their owner. Nowhere else. |
| `harvest` / `activate` | anyone | Closes the minute, shares its fees, activates new stakes. |
| `claim` | anyone | Pays a position what it is owed, to its owner. |
| `close_position` | anyone | Closes an empty position, pays what is left to its owner, returns the account rent to whoever paid it. |
| `rescue_token` | the admin | Sends back a *different* token that someone sent to a pool by mistake. Can never touch the pool's own coin. |
| `init_config` / `set_*` | the upgrade authority / admin | Sets the attestor, the fee receiver and the two flat fees (within their caps). |

## Repository

```
programs/fee-stake/   the program (Rust, Anchor)
crates/stake-math/    the minute-by-minute accounting, with property tests and recorded histories
sdk/                  TypeScript mirror of the accounting, PDA helpers, the program's IDL
tests/                end-to-end tests on a local validator (17 scenarios, attacks included)
tools/                check a coin or the program on mainnet, from your own machine
```

## Run the tests

Requirements: Rust (stable), [Solana CLI 2.x](https://docs.anza.xyz/cli/install), [Anchor 0.31.1](https://www.anchor-lang.com/docs/installation), Node 22.

```bash
npm install
npm run test:math     # accounting: property tests (Rust) + the TypeScript mirror replaying Rust histories to the lamport
npm run test:fuzz     # lock_fee_split: thousands of random and mutated payloads, only {pool: 100%} passes
npm run test:e2e      # 17 end-to-end scenarios on a local validator (~6 min: windows really last 60 s)
```

## Check a coin yourself

```bash
RPC_URL=https://api.mainnet-beta.solana.com npx tsx tools/check-coin.ts <coin mint address>
```

Prints the coin's staking pool, its creator address (must be this program's), pump.fun's fee-sharing setting (must be exactly the pool at 100%, with the admin revoked), and what the pool has paid. More in [VERIFY.md](VERIFY.md).

## License

Source-available: you may read, build, test and verify this code. See [LICENSE](LICENSE).
