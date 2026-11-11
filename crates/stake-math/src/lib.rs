//! Epoch-based pro-rata fee distribution to stakers.
//!
//! Time is cut into fixed windows (epochs) of `epoch_seconds` (1 min in
//! production), aligned on the clock: every n:00 of every minute.
//!
//! Rules:
//! * Fees received during an epoch are attributed, at the epoch boundary, to
//!   the positions that were **active for the whole epoch**.
//! * A deposit made during epoch E is *pending* and becomes active at the
//!   start of E+1. It therefore earns from the fees of E+1 onwards.
//! * An unstake request removes the tokens from the active set immediately
//!   (no share of the running epoch) and returns them after the cooldown.
//!
//! Example (1-min epochs): sent at 14:14:50 and held → earns from the
//! 14:15–14:16 window on, never from 14:14–14:15.
//!
//! Internally this is a reward-per-share accumulator that only advances at
//! epoch boundaries, plus a small ring of accumulator snapshots so a pending
//! deposit can be activated lazily (by anyone, any time later) with the exact
//! accumulator value of its activation boundary.
//!
//! No Solana dependency: host-testable and mirrored 1:1 in TypeScript.

#![forbid(unsafe_code)]

/// Accumulator scale. 1e18 keeps the per-share value exact to the lamport
/// for positions up to a whole pump.fun supply (1e15 base units): the rounding
/// error of a claim is `amount / PRECISION` < 0.001 lamport.
pub const PRECISION: u128 = 1_000_000_000_000_000_000; // 1e18
/// One-minute windows: fees are split every minute (n:00, n:01, …).
pub const DEFAULT_EPOCH_SECONDS: i64 = 60;
/// One hour between asking to leave and getting the tokens back (nothing is
/// earned meanwhile): the brake that keeps 1-minute windows from being farmed
/// by jumping in and out.
pub const DEFAULT_COOLDOWN_SECONDS: i64 = 60 * 60;
/// Snapshots of the accumulator kept for lazy activation (16 × 1 min = 16 min).
/// The crank activates pending deposits after every window close, so the ring
/// is a safety net for a crank outage: within it, a late activation still earns
/// exactly from its boundary; beyond it, from the oldest snapshot (never more).
/// Not larger: the whole StakePool is deserialized on the program's 4 KB stack,
/// and 64 snapshots (1 KB) make `init_pool` overflow it.
pub const ACC_RING: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StakeError {
    Overflow,
    ZeroAmount,
    InsufficientStake,
    CooldownActive,
    NothingToUnstake,
    InvalidParams,
}

type R<T> = Result<T, StakeError>;

/// `amount × acc / PRECISION` without overflowing the intermediate product.
///
/// `acc` grows without bound (lamports per base unit, × PRECISION) and `amount`
/// can be a whole supply; their plain product can exceed u128 even when the
/// result (lamports) is tiny — a 1-unit staker alone in the pool for one fat
/// window pushes `acc` past 1e24, after which `1e15 × acc` no longer fits.
/// Splitting at PRECISION keeps every intermediate within bounds for any
/// realistic pool: (acc / P) ≤ total lamports ever received, × amount ≤ 5e32.
fn mul_acc(amount: u64, acc: u128) -> R<u128> {
    let whole = (acc / PRECISION).checked_mul(amount as u128).ok_or(StakeError::Overflow)?;
    let frac = (acc % PRECISION).checked_mul(amount as u128).ok_or(StakeError::Overflow)? / PRECISION;
    whole.checked_add(frac).ok_or(StakeError::Overflow)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pool {
    pub epoch_seconds: i64,
    /// Index of the current epoch (`now / epoch_seconds`).
    pub epoch_index: u64,
    /// Tokens earning in the current epoch.
    pub total_active: u64,
    /// Tokens deposited this epoch; join `total_active` at the next boundary.
    pub total_pending: u64,
    /// Cumulative lamports per active token, × PRECISION.
    pub acc_reward_per_share: u128,
    /// `acc_reward_per_share` at the START of epoch i, in slot `i % ACC_RING`.
    pub acc_ring: [u128; ACC_RING],
    /// Lamports received during the current epoch, attributed at the boundary.
    pub carry: u64,
    /// Lamports attributed while nobody was active; paid with the next epoch.
    pub pending_rewards: u64,
    pub unclaimed_rewards: u64,
    pub total_distributed: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Position {
    /// Active (earning) tokens.
    pub amount: u64,
    /// Deposited this epoch, not yet earning.
    pub pending: u64,
    /// Epoch in which `pending` was deposited; eligible from `pending_epoch + 1`.
    pub pending_epoch: u64,
    pub reward_debt: u128,
    pub rewards_owed: u64,
    pub cooldown_amount: u64,
    pub cooldown_until: i64,
}

impl Pool {
    pub fn new(now: i64, epoch_seconds: i64) -> R<Self> {
        if epoch_seconds <= 0 {
            return Err(StakeError::InvalidParams);
        }
        Ok(Self {
            epoch_seconds,
            epoch_index: Self::epoch_of(now, epoch_seconds),
            total_active: 0,
            total_pending: 0,
            acc_reward_per_share: 0,
            acc_ring: [0; ACC_RING],
            carry: 0,
            pending_rewards: 0,
            unclaimed_rewards: 0,
            total_distributed: 0,
        })
    }

    pub fn epoch_of(ts: i64, epoch_seconds: i64) -> u64 {
        (ts.max(0) / epoch_seconds) as u64
    }

    /// Unix time at which the current epoch ends.
    pub fn epoch_end(&self) -> i64 {
        (self.epoch_index as i64 + 1) * self.epoch_seconds
    }

    /// Lamports landed on the pool: parked until the boundary.
    pub fn receive(&mut self, lamports: u64) -> R<()> {
        self.carry = self.carry.checked_add(lamports).ok_or(StakeError::Overflow)?;
        Ok(())
    }

    /// Advance to the epoch containing `now`. Must run before any other
    /// operation. Returns `true` if a boundary was crossed.
    pub fn sync(&mut self, now: i64) -> R<bool> {
        let cur = Self::epoch_of(now, self.epoch_seconds);
        if cur <= self.epoch_index {
            return Ok(false);
        }
        // 1. attribute the finished epoch's fees to the active set
        let total = self
            .carry
            .checked_add(self.pending_rewards)
            .ok_or(StakeError::Overflow)?;
        if total > 0 {
            if self.total_active == 0 {
                self.pending_rewards = total;
            } else {
                let delta = (total as u128)
                    .checked_mul(PRECISION)
                    .ok_or(StakeError::Overflow)?
                    / (self.total_active as u128);
                self.acc_reward_per_share = self
                    .acc_reward_per_share
                    .checked_add(delta)
                    .ok_or(StakeError::Overflow)?;
                self.unclaimed_rewards = self
                    .unclaimed_rewards
                    .checked_add(total)
                    .ok_or(StakeError::Overflow)?;
                self.total_distributed = self
                    .total_distributed
                    .checked_add(total)
                    .ok_or(StakeError::Overflow)?;
                self.pending_rewards = 0;
            }
            self.carry = 0;
        }
        // 2. pending deposits become active
        self.total_active = self
            .total_active
            .checked_add(self.total_pending)
            .ok_or(StakeError::Overflow)?;
        self.total_pending = 0;
        // 3. snapshot the accumulator for every epoch we just entered
        let first = self.epoch_index + 1;
        let start = if cur - first + 1 > ACC_RING as u64 { cur - ACC_RING as u64 + 1 } else { first };
        for e in start..=cur {
            self.acc_ring[(e % ACC_RING as u64) as usize] = self.acc_reward_per_share;
        }
        self.epoch_index = cur;
        Ok(true)
    }

    /// Accumulator at the start of `epoch`. If that snapshot has left the ring,
    /// the oldest one still held is returned: the position then earns from that
    /// epoch on (never more than its due — its weight was in the denominator
    /// since `epoch`, so the skipped part simply stays in the pool as dust).
    fn acc_at_epoch_start(&self, epoch: u64) -> Option<u128> {
        if epoch > self.epoch_index {
            return None;
        }
        let oldest = self.epoch_index.saturating_sub(ACC_RING as u64 - 1);
        let e = epoch.max(oldest);
        Some(self.acc_ring[(e % ACC_RING as u64) as usize])
    }

    /// Move earned rewards on the active amount into `rewards_owed`.
    fn settle(&self, p: &mut Position) -> R<()> {
        let accrued = mul_acc(p.amount, self.acc_reward_per_share)?;
        let earned = accrued.checked_sub(p.reward_debt).ok_or(StakeError::Overflow)?;
        p.rewards_owed = p
            .rewards_owed
            .checked_add(u64::try_from(earned).map_err(|_| StakeError::Overflow)?)
            .ok_or(StakeError::Overflow)?;
        p.reward_debt = accrued;
        Ok(())
    }

    /// Turn a pending deposit into an active one once its epoch has passed.
    /// Idempotent; anyone may trigger it. Uses the accumulator snapshot of the
    /// activation boundary so the deposit earns from that epoch on. If the
    /// snapshot has left the ring (nobody touched the position for
    /// `ACC_RING` epochs), the deposit starts earning now — it never over-pays.
    pub fn activate(&self, p: &mut Position) -> R<()> {
        if p.pending == 0 || self.epoch_index <= p.pending_epoch {
            return Ok(());
        }
        let eligible_from = p.pending_epoch + 1;
        let acc_at = self.acc_at_epoch_start(eligible_from).unwrap_or(self.acc_reward_per_share);
        self.settle(p)?;
        let earned = mul_acc(p.pending, self.acc_reward_per_share.saturating_sub(acc_at))?;
        p.rewards_owed = p
            .rewards_owed
            .checked_add(u64::try_from(earned).map_err(|_| StakeError::Overflow)?)
            .ok_or(StakeError::Overflow)?;
        p.amount = p.amount.checked_add(p.pending).ok_or(StakeError::Overflow)?;
        p.pending = 0;
        p.reward_debt = mul_acc(p.amount, self.acc_reward_per_share)?;
        Ok(())
    }

    /// Deposit: pending until the next boundary. `sync` must have run.
    pub fn stake(&mut self, p: &mut Position, amount: u64) -> R<()> {
        if amount == 0 {
            return Err(StakeError::ZeroAmount);
        }
        self.activate(p)?;
        p.pending = p.pending.checked_add(amount).ok_or(StakeError::Overflow)?;
        p.pending_epoch = self.epoch_index;
        self.total_pending = self.total_pending.checked_add(amount).ok_or(StakeError::Overflow)?;
        Ok(())
    }

    /// Take owed rewards out. Returns lamports to pay.
    ///
    /// Capped by what was attributed and not yet claimed: per-position
    /// rounding (`floor(a·acc₂) − floor(a·acc₁)` can exceed
    /// `floor(a·(acc₂−acc₁))` by one) could otherwise let the sum of all
    /// claims exceed the fees received by a lamport or two. The pool never
    /// pays out more than it took in; a position may be a lamport short.
    pub fn claim(&mut self, p: &mut Position) -> R<u64> {
        self.activate(p)?;
        self.settle(p)?;
        let owed = p.rewards_owed.min(self.unclaimed_rewards);
        p.rewards_owed -= owed;
        self.unclaimed_rewards -= owed;
        Ok(owed)
    }

    /// Stop earning on `amount` now; withdrawable after `cooldown_seconds`.
    /// Draws from the pending deposit first, then from the active amount.
    pub fn request_unstake(&mut self, p: &mut Position, amount: u64, now: i64, cooldown_seconds: i64) -> R<()> {
        if amount == 0 {
            return Err(StakeError::ZeroAmount);
        }
        self.activate(p)?;
        let available = p.amount.checked_add(p.pending).ok_or(StakeError::Overflow)?;
        if amount > available {
            return Err(StakeError::InsufficientStake);
        }
        let from_pending = amount.min(p.pending);
        if from_pending > 0 {
            p.pending -= from_pending;
            self.total_pending -= from_pending;
        }
        let from_active = amount - from_pending;
        if from_active > 0 {
            self.settle(p)?;
            p.amount -= from_active;
            self.total_active -= from_active;
            p.reward_debt = mul_acc(p.amount, self.acc_reward_per_share)?;
        }
        p.cooldown_amount = p.cooldown_amount.checked_add(amount).ok_or(StakeError::Overflow)?;
        p.cooldown_until = now.checked_add(cooldown_seconds).ok_or(StakeError::Overflow)?;
        Ok(())
    }

    /// Everything the position holds (active + pending). For intent-based full unstake.
    pub fn staked_total(p: &Position) -> u64 {
        p.amount.saturating_add(p.pending)
    }

    /// Withdraw cooled-down tokens. Returns the token amount.
    pub fn unstake(&mut self, p: &mut Position, now: i64) -> R<u64> {
        if p.cooldown_amount == 0 {
            return Err(StakeError::NothingToUnstake);
        }
        if now < p.cooldown_until {
            return Err(StakeError::CooldownActive);
        }
        let out = p.cooldown_amount;
        p.cooldown_amount = 0;
        p.cooldown_until = 0;
        Ok(out)
    }

    /// Claimable right now (view).
    pub fn pending_rewards_of(&self, p: &Position) -> u64 {
        let mut q = *p;
        if self.activate(&mut q).is_err() {
            return p.rewards_owed;
        }
        let accrued = match mul_acc(q.amount, self.acc_reward_per_share) { Ok(a) => a, Err(_) => return p.rewards_owed };
        let earned = accrued.saturating_sub(q.reward_debt);
        q.rewards_owed.saturating_add(u64::try_from(earned).unwrap_or(u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_000_000;
    const SOL: u64 = 1_000_000_000;
    const E: i64 = 900;
    /// n:MM → seconds, epoch k.
    fn at(epoch: i64, min: i64, sec: i64) -> i64 {
        epoch * E + min * 60 + sec
    }

    #[test]
    fn spec_example_stake_at_07_hold_until_31() {
        // B is active since epoch 0 so epoch-1 fees have a taker; A is the user of the spec.
        let mut pool = Pool::new(0, E).unwrap();
        let mut a = Position::default();
        let mut b = Position::default();
        pool.stake(&mut b, 5 * T).unwrap();
        pool.sync(at(1, 0, 0)).unwrap(); // B active for epoch 1 (n:00–n:15)

        pool.stake(&mut a, 5 * T).unwrap(); // A at n:07 → pending
        pool.sync(at(1, 7, 0)).unwrap();
        pool.receive(SOL).unwrap(); // fees of window n:00–n:15
        pool.sync(at(2, 0, 0)).unwrap(); // boundary n:15
        assert_eq!(pool.pending_rewards_of(&b), SOL, "B alone gets the n:00–n:15 window");
        assert_eq!(pool.pending_rewards_of(&a), 0, "A was not active for the whole window");

        pool.receive(SOL).unwrap(); // fees of window n:15–n:30
        pool.sync(at(3, 0, 0)).unwrap(); // boundary n:30
        assert_eq!(pool.pending_rewards_of(&a), SOL / 2, "A gets its share of n:15–n:30");
        assert_eq!(pool.pending_rewards_of(&b), SOL + SOL / 2);

        // A requests unstake at n:31 → out of the n:30–n:45 window
        pool.sync(at(3, 1, 0)).unwrap();
        pool.request_unstake(&mut a, 5 * T, at(3, 1, 0), 15 * 60).unwrap(); // the 15-min example of the spec
        pool.receive(SOL).unwrap();
        pool.sync(at(4, 0, 0)).unwrap();
        assert_eq!(pool.pending_rewards_of(&a), SOL / 2);
        assert_eq!(pool.pending_rewards_of(&b), 2 * SOL + SOL / 2);

        assert_eq!(pool.unstake(&mut a, at(3, 15, 59)), Err(StakeError::CooldownActive));
        assert_eq!(pool.unstake(&mut a, at(3, 16, 0)).unwrap(), 5 * T); // 15 min after n:31
        assert_eq!(pool.claim(&mut a).unwrap(), SOL / 2);
    }

    #[test]
    fn production_defaults_one_minute_windows_one_hour_cooldown() {
        // sent at second 50 of minute 0, held: earns the whole of minute 1; leaves at 2:10, tokens at 62:10
        let e = DEFAULT_EPOCH_SECONDS;
        assert_eq!((e, DEFAULT_COOLDOWN_SECONDS), (60, 3600));
        let mut pool = Pool::new(0, e).unwrap();
        let mut a = Position::default();
        pool.sync(50).unwrap();
        pool.stake(&mut a, 5 * T).unwrap();
        pool.receive(SOL).unwrap(); // fees of minute 0: nobody was active the whole minute
        pool.sync(60).unwrap();
        assert_eq!(pool.pending_rewards_of(&a), 0, "not active during minute 0");
        pool.receive(SOL).unwrap(); // fees of minute 1
        pool.sync(120).unwrap();
        assert_eq!(pool.pending_rewards_of(&a), 2 * SOL, "minute 1, plus minute 0 that had no taker");
        pool.sync(130).unwrap();
        pool.request_unstake(&mut a, 5 * T, 130, DEFAULT_COOLDOWN_SECONDS).unwrap();
        assert_eq!(pool.unstake(&mut a, 130 + 3599), Err(StakeError::CooldownActive));
        assert_eq!(pool.unstake(&mut a, 130 + 3600).unwrap(), 5 * T);
    }

    #[test]
    fn lazy_activation_uses_the_boundary_snapshot() {
        let mut pool = Pool::new(0, E).unwrap();
        let mut a = Position::default();
        let mut b = Position::default();
        pool.stake(&mut b, T).unwrap();
        pool.sync(at(1, 0, 0)).unwrap();
        pool.stake(&mut a, T).unwrap(); // epoch 1, active from epoch 2
        // nobody touches A for 3 epochs; fees every epoch
        for e in 2..=4 {
            pool.receive(SOL).unwrap();
            pool.sync(at(e, 0, 0)).unwrap();
        }
        // epoch1 fees → B alone; epochs 2,3 fees → split
        assert_eq!(pool.pending_rewards_of(&a), SOL);
        assert_eq!(pool.pending_rewards_of(&b), 2 * SOL);
        pool.activate(&mut a).unwrap();
        assert_eq!(a.amount, T);
        assert_eq!(a.pending, 0);
        assert_eq!(pool.claim(&mut a).unwrap(), SOL);
    }

    #[test]
    fn snapshot_out_of_ring_never_over_pays() {
        let mut pool = Pool::new(0, E).unwrap();
        let mut a = Position::default();
        let mut b = Position::default();
        pool.stake(&mut b, T).unwrap();
        pool.sync(at(1, 0, 0)).unwrap();
        pool.stake(&mut a, T).unwrap();
        pool.sync(at(2, 0, 0)).unwrap();
        pool.activate(&mut b).unwrap(); // the crank activated B in time, A is left alone
        let mut received = 0u64;
        for e in 3..(3 + ACC_RING as i64 + 5) {
            pool.receive(SOL).unwrap();
            received += SOL;
            pool.sync(at(e, 0, 0)).unwrap();
        }
        let pa = pool.claim(&mut a).unwrap();
        let pb = pool.claim(&mut b).unwrap();
        assert!(pa + pb <= received, "paid {} > received {}", pa + pb, received);
        assert!(pa < pb, "A ({pa}) lost the epochs that fell out of the ring, B ({pb}) did not");
        assert!(pa >= (ACC_RING as u64 - 2) * SOL / 2, "A still gets the ring's worth: {pa}");
    }

    #[test]
    fn fees_with_no_active_staker_wait_for_the_first_active_set() {
        let mut pool = Pool::new(0, E).unwrap();
        pool.receive(3 * SOL).unwrap();
        pool.sync(at(1, 0, 0)).unwrap();
        assert_eq!(pool.pending_rewards, 3 * SOL);
        let mut a = Position::default();
        pool.stake(&mut a, T).unwrap();
        pool.sync(at(2, 0, 0)).unwrap(); // A active, but fees only attributed at the NEXT boundary
        pool.sync(at(3, 0, 0)).unwrap();
        assert_eq!(pool.pending_rewards_of(&a), 3 * SOL);
    }

    #[test]
    fn unstake_draws_from_pending_first_and_partial_keeps_earning() {
        let mut pool = Pool::new(0, E).unwrap();
        let mut a = Position::default();
        pool.stake(&mut a, 4 * T).unwrap();
        pool.sync(at(1, 0, 0)).unwrap();
        pool.stake(&mut a, 2 * T).unwrap(); // pending 2
        pool.request_unstake(&mut a, 3 * T, at(1, 1, 0), 900).unwrap(); // 2 from pending, 1 from active
        assert_eq!(a.pending, 0);
        assert_eq!(a.amount, 3 * T);
        assert_eq!(pool.total_active, 3 * T);
        assert_eq!(pool.total_pending, 0);
        pool.receive(SOL).unwrap();
        pool.sync(at(2, 0, 0)).unwrap();
        assert!(pool.pending_rewards_of(&a) >= SOL - 1); // 1 lamport of rounding
    }

    #[test]
    fn two_equal_stakers_split_50_50() {
        let mut pool = Pool::new(0, E).unwrap();
        let mut a = Position::default();
        let mut b = Position::default();
        pool.stake(&mut a, 5 * T).unwrap();
        pool.stake(&mut b, 5 * T).unwrap();
        pool.sync(at(1, 0, 0)).unwrap();
        pool.receive(SOL).unwrap();
        pool.sync(at(2, 0, 0)).unwrap();
        assert_eq!(pool.claim(&mut a).unwrap(), SOL / 2);
        assert_eq!(pool.claim(&mut b).unwrap(), SOL / 2);
        assert_eq!(pool.unclaimed_rewards, 0);
    }

    #[test]
    fn never_over_pays_with_rounding() {
        let mut pool = Pool::new(0, E).unwrap();
        let amounts = [1u64, 3, 7, 11, 13, 999_999, 123_456_789];
        let mut ps: Vec<Position> = amounts.iter().map(|_| Position::default()).collect();
        for (p, a) in ps.iter_mut().zip(amounts) {
            pool.stake(p, a).unwrap();
        }
        pool.sync(at(1, 0, 0)).unwrap();
        let mut total_in = 0u64;
        for (i, fee) in [1u64, 2, 999, 1_000_001, 123_456_789, 5 * SOL].iter().enumerate() {
            pool.receive(*fee).unwrap();
            total_in += fee;
            pool.sync(at(2 + i as i64, 0, 0)).unwrap();
        }
        let paid: u64 = ps.iter_mut().map(|p| pool.claim(p).unwrap()).sum();
        assert!(paid <= total_in);
        assert!(total_in - paid <= 7 * 6);
    }

    #[test]
    fn multi_epoch_gap_is_handled_in_one_sync() {
        let mut pool = Pool::new(0, E).unwrap();
        let mut a = Position::default();
        pool.stake(&mut a, T).unwrap();
        pool.sync(at(1, 0, 0)).unwrap();
        pool.receive(SOL).unwrap();
        assert!(pool.sync(at(9, 3, 0)).unwrap()); // 8 epochs at once
        assert_eq!(pool.epoch_index, 9);
        assert_eq!(pool.pending_rewards_of(&a), SOL);
        assert_eq!(pool.acc_at_epoch_start(9), Some(pool.acc_reward_per_share));
    }
}
