/**
 * Epoch-based fee distribution — exact BigInt mirror of `crates/stake-math`.
 *
 * Fees received during a window (1 min, aligned on the clock) are attributed
 * at the boundary to positions active for the whole window. A deposit made
 * during window E becomes active at the start of E+1. An unstake request
 * stops earning immediately; tokens come back after the cooldown.
 */

export const PRECISION = 1_000_000_000_000_000_000n; // 1e18, mirrors stake-math
export const DEFAULT_EPOCH_SECONDS = 60;
export const DEFAULT_COOLDOWN_SECONDS = 60 * 60;
export const ACC_RING = 16;

export interface StakePool {
  epochSeconds: number;
  epochIndex: number;
  totalActive: bigint;
  totalPending: bigint;
  accRewardPerShare: bigint;
  accRing: bigint[]; // length ACC_RING
  carry: bigint;
  pendingRewards: bigint;
  unclaimedRewards: bigint;
  totalDistributed: bigint;
}

export interface StakePosition {
  amount: bigint;
  pending: bigint;
  pendingEpoch: number;
  rewardDebt: bigint;
  rewardsOwed: bigint;
  cooldownAmount: bigint;
  cooldownUntil: number;
  /** SOL pushed to the owner so far, lamports. */
  totalClaimed: bigint;
}

export const epochOf = (ts: number, epochSeconds: number) => Math.floor(Math.max(0, ts) / epochSeconds);
export const epochEnd = (pool: StakePool) => (pool.epochIndex + 1) * pool.epochSeconds;

export const newPool = (now: number, epochSeconds = DEFAULT_EPOCH_SECONDS): StakePool => ({
  epochSeconds,
  epochIndex: epochOf(now, epochSeconds),
  totalActive: 0n,
  totalPending: 0n,
  accRewardPerShare: 0n,
  accRing: Array.from({ length: ACC_RING }, () => 0n),
  carry: 0n,
  pendingRewards: 0n,
  unclaimedRewards: 0n,
  totalDistributed: 0n,
});

export const emptyPosition = (): StakePosition => ({
  amount: 0n,
  pending: 0n,
  pendingEpoch: 0,
  rewardDebt: 0n,
  rewardsOwed: 0n,
  cooldownAmount: 0n,
  cooldownUntil: 0,
  totalClaimed: 0n,
});

/** Lamports landed on the pool: parked until the boundary. */
export function receive(pool: StakePool, lamports: bigint): void {
  pool.carry += lamports;
}

/** Advance to the epoch containing `now`. Returns true if a boundary was crossed. */
export function sync(pool: StakePool, now: number): boolean {
  const cur = epochOf(now, pool.epochSeconds);
  if (cur <= pool.epochIndex) return false;
  const total = pool.carry + pool.pendingRewards;
  if (total > 0n) {
    if (pool.totalActive === 0n) {
      pool.pendingRewards = total;
    } else {
      pool.accRewardPerShare += (total * PRECISION) / pool.totalActive;
      pool.unclaimedRewards += total;
      pool.totalDistributed += total;
      pool.pendingRewards = 0n;
    }
    pool.carry = 0n;
  }
  pool.totalActive += pool.totalPending;
  pool.totalPending = 0n;
  const first = pool.epochIndex + 1;
  const ring = pool.accRing.length; // = ACC_RING; read from the data so a decoded account always matches
  const start = cur - first + 1 > ring ? cur - ring + 1 : first;
  for (let e = start; e <= cur; e++) pool.accRing[e % ring] = pool.accRewardPerShare;
  pool.epochIndex = cur;
  return true;
}

const accAtEpochStart = (pool: StakePool, epoch: number): bigint | null => {
  if (epoch > pool.epochIndex) return null;
  const ring = pool.accRing.length;
  const oldest = Math.max(0, pool.epochIndex - (ring - 1));
  return pool.accRing[Math.max(epoch, oldest) % ring];
};

const settle = (pool: StakePool, p: StakePosition) => {
  const accrued = (p.amount * pool.accRewardPerShare) / PRECISION;
  p.rewardsOwed += accrued - p.rewardDebt;
  p.rewardDebt = accrued;
};

/** Turn a pending deposit into an active one once its epoch has passed. Idempotent. */
export function activate(pool: StakePool, p: StakePosition): void {
  if (p.pending === 0n || pool.epochIndex <= p.pendingEpoch) return;
  const accAt = accAtEpochStart(pool, p.pendingEpoch + 1) ?? pool.accRewardPerShare;
  settle(pool, p);
  const d = pool.accRewardPerShare > accAt ? pool.accRewardPerShare - accAt : 0n;
  p.rewardsOwed += (p.pending * d) / PRECISION;
  p.amount += p.pending;
  p.pending = 0n;
  p.rewardDebt = (p.amount * pool.accRewardPerShare) / PRECISION;
}

/** Deposit: pending until the next boundary. `sync` must have run. */
export function stake(pool: StakePool, p: StakePosition, amount: bigint): void {
  if (amount <= 0n) throw new Error("Zero amount");
  activate(pool, p);
  p.pending += amount;
  p.pendingEpoch = pool.epochIndex;
  pool.totalPending += amount;
}

export function claim(pool: StakePool, p: StakePosition): bigint {
  activate(pool, p);
  settle(pool, p);
  // capped by what was attributed and not yet claimed (mirrors stake-math: the pool never over-pays)
  const owed = p.rewardsOwed < pool.unclaimedRewards ? p.rewardsOwed : pool.unclaimedRewards;
  p.rewardsOwed -= owed;
  pool.unclaimedRewards -= owed;
  p.totalClaimed += owed;
  return owed;
}

export function requestUnstake(pool: StakePool, p: StakePosition, amount: bigint, now: number, cooldownSeconds = DEFAULT_COOLDOWN_SECONDS): void {
  if (amount <= 0n) throw new Error("Zero amount");
  activate(pool, p);
  if (amount > p.amount + p.pending) throw new Error("Not enough staked tokens");
  const fromPending = amount < p.pending ? amount : p.pending;
  if (fromPending > 0n) {
    p.pending -= fromPending;
    pool.totalPending -= fromPending;
  }
  const fromActive = amount - fromPending;
  if (fromActive > 0n) {
    settle(pool, p);
    p.amount -= fromActive;
    pool.totalActive -= fromActive;
    p.rewardDebt = (p.amount * pool.accRewardPerShare) / PRECISION;
  }
  p.cooldownAmount += amount;
  p.cooldownUntil = now + cooldownSeconds;
}

export const stakedTotal = (p: StakePosition) => p.amount + p.pending;

export function unstake(p: StakePosition, now: number): bigint {
  if (p.cooldownAmount === 0n) throw new Error("Nothing to unstake");
  if (now < p.cooldownUntil) throw new Error("Cooldown still running");
  const out = p.cooldownAmount;
  p.cooldownAmount = 0n;
  p.cooldownUntil = 0;
  return out;
}

/** Claimable right now (view, no mutation). */
export function pending(pool: StakePool, p: StakePosition): bigint {
  const q = { ...p };
  activate(pool, q);
  const accrued = (q.amount * pool.accRewardPerShare) / PRECISION;
  return q.rewardsOwed + (accrued > q.rewardDebt ? accrued - q.rewardDebt : 0n);
}

/** Share of the next window's fees in [0, 1] (active tokens only). */
export const shareOf = (pool: StakePool, p: StakePosition): number => {
  const q = { ...p };
  activate(pool, q);
  const total = pool.totalActive;
  return total === 0n ? 0 : Number(q.amount) / Number(total);
};
