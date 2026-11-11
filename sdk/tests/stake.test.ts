// Run with: node --experimental-strip-types --test sdk/tests/stake.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ACC_RING,
  DEFAULT_COOLDOWN_SECONDS,
  activate,
  claim,
  emptyPosition,
  newPool,
  pending,
  receive,
  requestUnstake,
  stake,
  sync,
  unstake,
} from "../stake.ts";

const T = 1_000_000n;
const SOL = 1_000_000_000n;
const E = 900;
const at = (epoch: number, min: number, sec = 0) => epoch * E + min * 60 + sec;

test("spec example: stake at n:07, hold until n:31 → paid for n:15–n:30 only", () => {
  const pool = newPool(0, E);
  const a = emptyPosition();
  const b = emptyPosition();
  stake(pool, b, 5n * T);
  sync(pool, at(1, 0));
  stake(pool, a, 5n * T); // n:07, pending
  sync(pool, at(1, 7));
  receive(pool, SOL); // window n:00–n:15
  sync(pool, at(2, 0));
  assert.equal(pending(pool, b), SOL);
  assert.equal(pending(pool, a), 0n);
  receive(pool, SOL); // window n:15–n:30
  sync(pool, at(3, 0));
  assert.equal(pending(pool, a), SOL / 2n);
  sync(pool, at(3, 1));
  requestUnstake(pool, a, 5n * T, at(3, 1), 15 * 60); // n:31, 15-min cooldown as in the original spec
  receive(pool, SOL);
  sync(pool, at(4, 0));
  assert.equal(pending(pool, a), SOL / 2n);
  assert.equal(pending(pool, b), 2n * SOL + SOL / 2n);
  assert.throws(() => unstake(a, at(3, 15, 59)));
  assert.equal(unstake(a, at(3, 16)), 5n * T);
  assert.equal(claim(pool, a), SOL / 2n);
});

test("lazy activation uses the boundary snapshot", () => {
  const pool = newPool(0, E);
  const a = emptyPosition();
  const b = emptyPosition();
  stake(pool, b, T);
  sync(pool, at(1, 0));
  stake(pool, a, T);
  for (let e = 2; e <= 4; e++) {
    receive(pool, SOL);
    sync(pool, at(e, 0));
  }
  assert.equal(pending(pool, a), SOL);
  assert.equal(pending(pool, b), 2n * SOL);
  activate(pool, a);
  assert.equal(a.amount, T);
  assert.equal(claim(pool, a), SOL);
});

test("snapshot out of ring never over-pays", () => {
  const pool = newPool(0, E);
  const a = emptyPosition();
  const b = emptyPosition();
  stake(pool, b, T);
  sync(pool, at(1, 0));
  stake(pool, a, T);
  sync(pool, at(2, 0));
  activate(pool, b);
  let received = 0n;
  for (let e = 3; e < 3 + ACC_RING + 5; e++) {
    receive(pool, SOL);
    received += SOL;
    sync(pool, at(e, 0));
  }
  const pa = claim(pool, a);
  const pb = claim(pool, b);
  assert.ok(pa + pb <= received);
  assert.ok(pa < pb);
});

test("two equal stakers split 50/50, no over-pay", () => {
  const pool = newPool(0, E);
  const a = emptyPosition();
  const b = emptyPosition();
  stake(pool, a, 5n * T);
  stake(pool, b, 5n * T);
  sync(pool, at(1, 0));
  receive(pool, SOL);
  sync(pool, at(2, 0));
  assert.equal(claim(pool, a), SOL / 2n);
  assert.equal(claim(pool, b), SOL / 2n);
  assert.equal(pool.unclaimedRewards, 0n);
});

test("unstake draws from pending first", () => {
  const pool = newPool(0, E);
  const a = emptyPosition();
  stake(pool, a, 4n * T);
  sync(pool, at(1, 0));
  stake(pool, a, 2n * T);
  requestUnstake(pool, a, 3n * T, at(1, 1), 900);
  assert.equal(a.pending, 0n);
  assert.equal(a.amount, 3n * T);
  assert.equal(pool.totalActive, 3n * T);
  assert.equal(pool.totalPending, 0n);
});

test("production: 1-hour cooldown — tokens come back exactly 3600 s after the exit, not before", () => {
  const pool = newPool(0, 60);
  const p = emptyPosition();
  stake(pool, p, 5_000n * T);
  sync(pool, 60);
  requestUnstake(pool, p, 5_000n * T, 1000); // the attestor runs request_exit at t=1000
  assert.equal(p.cooldownUntil, 1000 + DEFAULT_COOLDOWN_SECONDS);
  assert.throws(() => unstake(p, 1000 + 3599));
  assert.equal(unstake(p, 1000 + 3600), 5_000n * T);
  assert.equal(p.cooldownAmount, 0n);
});
