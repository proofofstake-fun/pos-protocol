# Verify it yourself

Three things to check, none of which needs to trust us.

## 1. The program on Solana is this code

The program is built with Anchor's *verifiable* build (a pinned Docker image), so
anyone gets the exact same bytes from the same commit.

```bash
cargo install solana-verify
solana-verify verify-from-repo -u mainnet-beta \
  --program-id PoSBsxAZVhLmPAUv9YLcWHauVjzSro6smzt92JPWtLY \
  https://github.com/proofofstake-fun/pos-protocol \
  --library-name fee_stake
```

Or by hand:

```bash
anchor build --verifiable -p fee_stake
sha256sum target/verifiable/fee_stake.so
solana-verify get-program-hash -u mainnet-beta PoSBsxAZVhLmPAUv9YLcWHauVjzSro6smzt92JPWtLY
```

The two hashes must be identical.

## 2. Who can still change the program

```bash
RPC_URL=https://api.mainnet-beta.solana.com npx tsx tools/check-program.ts
```

Prints the upgrade authority (the key that could deploy a new version), the
admin, the attestor, the wallet that receives the platform's flat fees, and
their current amounts. If the upgrade authority is `none`, the program can never
change again.

## 3. A coin's fees really go to its stakers

```bash
RPC_URL=https://api.mainnet-beta.solana.com npx tsx tools/check-coin.ts <coin mint>
```

What it checks, and what you should see:

| Check | Expected |
|---|---|
| The coin's pump.fun creator | `["creator", mint]` of this program: an address with no private key |
| pump.fun fee sharing: shareholders | exactly one: the coin's pool, 10,000 bps (100%) |
| pump.fun fee sharing: admin revoked | `true`: nobody can ever change it again |
| The pool | exists, with its staked total and what it has paid |

You can also look at the fee-sharing account on any explorer:
its address is pump.fun's `["sharing-config", mint]` PDA, printed by the tool.

## 4. Every payout is a transaction

Every minute's payouts are ordinary Solana transactions from the pool to each
staker's wallet. Open the pool address on an explorer to see them all.
