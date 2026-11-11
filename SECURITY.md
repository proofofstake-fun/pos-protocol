# Trust model

What you do **not** have to trust, and the few things you do. We would rather
say it here than have you find out.

## Guaranteed by the program

- **The fee split cannot move.** It is written once as 100% to the coin's pool, then locked by pump.fun's own program (`admin_revoked`). Not the launcher, not us, not a future version of this program can change it.
- **Your coins only go back to you.** The pool address is a program address with no private key. Coins leave it only to the wallet that owns the position.
- **SOL is only paid to you.** A payout goes to the position's owner. Whoever sends it keeps at most the network fee (≤ 5,000 lamports), nothing when you claim yourself.
- **No platform cut on trading.** The program accepts no shareholder other than the pool. The platform's two flat fees are capped in the code (launch ≤ 0.2 SOL, exit ≤ 0.05 SOL).
- **No timing games.** A stake earns from the next full minute; leaving stops earnings immediately and locks the coins for one hour.

## What remains a matter of trust

1. **The upgrade authority.** Until it is handed to a multisig or removed, the key that deployed the program could deploy a new version. `tools/check-program.ts` shows its current state. This is the one assumption that no amount of code review removes, and we will say publicly when it changes.
2. **The admin** can rotate the attestor, change the fee receiver and the two flat fees (within their caps), and send back a *foreign* token sent to a pool by mistake (`rescue_token`, never the pool's own coin). It cannot touch staked coins or SOL owed to stakers.
3. **The attestor, only for people who stake without connecting a wallet.** On Solana, a program cannot see who sent it a plain token transfer. So when someone stakes by *sending* coins to the pool address, an off-chain service reads the sender from the transaction and attests it (`credit_deposit` / `request_exit`). The program bounds this:
   - it never credits more than the pool actually received;
   - it only ever returns coins and SOL to the credited wallet;
   - every attestation is published on-chain with the signature of the transfer it comes from, so anyone can check it.

   If the attestor were wrong or compromised, a deposit *by transfer* could be credited to the wrong wallet, or a staker could be put in the 1-hour exit (their coins still come back to them). **Staking with a connected wallet does not use the attestor at all.**
4. **The payout service.** Payouts are pushed every minute by a service we run. Every step it takes is permissionless except the attestation above: anyone could run it. If it stopped, nothing would be lost; amounts owed stay owed, and a connected wallet can claim, unstake and withdraw on its own.
5. **pump.fun** keeps its own administrative powers over its programs, for every coin on pump.fun.

## Tests

- `crates/stake-math`: property tests (thousands of random histories: every lamport accounted for, never more promised than received, an exact pro-rata split, no overflow even with a whale next to a 1-token staker) and a TypeScript mirror checked against Rust to the lamport.
- `programs/fee-stake` (`split_fuzz`): random and mutated fee-sharing payloads; only `{pool: 100%}` passes.
- `tests/fee_stake.ts`: 17 end-to-end scenarios on a local validator, including every attempt to move someone else's coins or SOL.

## Reporting a vulnerability

Please report privately before publishing anything, and never test on other people's funds: https://x.com/pos_fun. We will answer, fix, and credit you if you wish.
