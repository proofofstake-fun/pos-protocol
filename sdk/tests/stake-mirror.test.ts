// Run with: node --experimental-strip-types --test sdk/tests/stake-mirror.test.ts
//
// The crank decides payouts and the site shows what is owed with the TypeScript
// copy of `crates/stake-math` (sdk/src/stake.ts); the program uses the Rust one.
// This replays random histories produced by the Rust crate (examples/histories.rs)
// and requires the TypeScript copy to land on the same numbers, to the lamport,
// after every single step.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { DEFAULT_COOLDOWN_SECONDS, activate, claim, emptyPosition, newPool, pending, receive, requestUnstake, stake, stakedTotal, sync, unstake } from "../stake.ts";

type Step = { op: string; user: number; arg: string; now: number; epoch: number; active: string; pending: string; acc: string; carry: string; waiting: string; unclaimed: string; distributed: string; owed: string[] };

test("the TypeScript mirror matches the program's math on 300 random histories", () => {
  const raw = execFileSync("cargo", ["run", "-q", "--release", "-p", "stake-math", "--example", "histories", "300"], { encoding: "utf8", maxBuffer: 1 << 28 });
  const histories: { start: number; steps: Step[] }[] = JSON.parse(raw);
  let checked = 0;
  for (const [h, { start, steps }] of histories.entries()) {
    const pool = newPool(start);
    const pos = Array.from({ length: 5 }, emptyPosition);
    let now = start;
    for (const [i, s] of steps.entries()) {
      const p = pos[s.user];
      const arg = BigInt(s.arg);
      const where = `history ${h}, step ${i} (${s.op})`;
      switch (s.op) {
        case "wait": now += Number(arg); break;
        case "stake": sync(pool, now); stake(pool, p, arg); break;
        case "fees": receive(pool, arg); sync(pool, now); break;
        case "exit": { sync(pool, now); const q = structuredClone(p); activate(pool, q); const t = stakedTotal(q); assert.equal(t, arg, where); if (t > 0n) requestUnstake(pool, p, t, now, DEFAULT_COOLDOWN_SECONDS); break; }
        case "claim": sync(pool, now); assert.equal(claim(pool, p), arg, where); break;
        case "unstake": { sync(pool, now); let got = 0n; try { got = unstake(p, now); } catch { got = 0n; } assert.equal(got, arg, where); break; }
        case "activate": sync(pool, now); activate(pool, p); break;
      }
      assert.equal(now, s.now, where);
      assert.equal(pool.epochIndex, s.epoch, `${where}: window`);
      assert.equal(pool.totalActive, BigInt(s.active), `${where}: active`);
      assert.equal(pool.totalPending, BigInt(s.pending), `${where}: pending`);
      assert.equal(pool.accRewardPerShare, BigInt(s.acc), `${where}: accumulator`);
      assert.equal(pool.carry, BigInt(s.carry), `${where}: carry`);
      assert.equal(pool.pendingRewards, BigInt(s.waiting), `${where}: waiting`);
      assert.equal(pool.unclaimedRewards, BigInt(s.unclaimed), `${where}: unclaimed`);
      assert.equal(pool.totalDistributed, BigInt(s.distributed), `${where}: distributed`);
      s.owed.forEach((o, u) => assert.equal(pending(pool, pos[u]), BigInt(o), `${where}: owed to user ${u}`));
      checked++;
    }
  }
  assert.ok(checked > 10_000, `only ${checked} steps checked`);
});
