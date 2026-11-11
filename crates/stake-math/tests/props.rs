//! Property tests for the reward accumulator. Random sequences of operations
//! on up to four positions; after every step the accounting identities must
//! hold, the pool must never pay more than it received, and nothing may
//! overflow for the magnitudes a pump.fun coin can reach (1e15 base units of
//! supply, hundreds of SOL per window).
//!
//! `cargo test -p stake-math` runs them (256 cases each by default; set
//! PROPTEST_CASES=5000 for a longer run before an audit).

use proptest::prelude::*;
use stake_math::{Pool, Position, StakeError, ACC_RING, PRECISION};

const E: i64 = 900;
/// Whole pump.fun supply: 1e9 tokens × 1e6 decimals.
const SUPPLY: u64 = 1_000_000_000_000_000;
const SOL: u64 = 1_000_000_000;

#[derive(Debug, Clone)]
enum Op {
    Stake { who: usize, amount: u64 },
    Unstake { who: usize, amount: u64 },
    Withdraw { who: usize },
    Claim { who: usize },
    Fees(u64),
    Advance(u8),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0..4usize, 1..=SUPPLY / 4).prop_map(|(who, amount)| Op::Stake { who, amount }),
        (0..4usize, 1..=SUPPLY / 4).prop_map(|(who, amount)| Op::Unstake { who, amount }),
        (0..4usize).prop_map(|who| Op::Withdraw { who }),
        (0..4usize).prop_map(|who| Op::Claim { who }),
        (0..=500 * SOL).prop_map(Op::Fees),
        (1..=(ACC_RING as u8 + 2)).prop_map(Op::Advance),
    ]
}

struct World {
    pool: Pool,
    pos: [Position; 4],
    now: i64,
    received: u64,
    claimed: u64,
    last_acc: u128,
}

impl World {
    fn new() -> Self {
        Self { pool: Pool::new(0, E).unwrap(), pos: [Position::default(); 4], now: 0, received: 0, claimed: 0, last_acc: 0 }
    }

    fn apply(&mut self, op: &Op) {
        self.pool.sync(self.now).unwrap();
        let r: Result<(), StakeError> = match *op {
            Op::Stake { who, amount } => {
                // cannot stake more than the supply in total
                let staked: u64 = self.pos.iter().map(|p| p.amount + p.pending + p.cooldown_amount).sum();
                if staked.saturating_add(amount) > SUPPLY { return; }
                self.pool.stake(&mut self.pos[who], amount)
            }
            Op::Unstake { who, amount } => self.pool.request_unstake(&mut self.pos[who], amount, self.now, E).or_else(|e| match e {
                StakeError::InsufficientStake | StakeError::ZeroAmount => Ok(()),
                e => Err(e),
            }),
            Op::Withdraw { who } => self.pool.unstake(&mut self.pos[who], self.now).map(|_| ()).or_else(|e| match e {
                StakeError::NothingToUnstake | StakeError::CooldownActive => Ok(()),
                e => Err(e),
            }),
            Op::Claim { who } => self.pool.claim(&mut self.pos[who]).map(|owed| { self.claimed += owed; }),
            Op::Fees(l) => {
                if self.received.checked_add(l).is_none() { return; }
                self.received += l;
                self.pool.receive(l)
            }
            Op::Advance(n) => {
                self.now += n as i64 * E;
                self.pool.sync(self.now).map(|_| ())
            }
        };
        assert!(r.is_ok(), "{op:?} failed with {r:?} at {:?}", self.pool);
        self.check(op);
    }

    fn check(&mut self, op: &Op) {
        let p = &self.pool;
        // 1. the accumulator only ever grows
        assert!(p.acc_reward_per_share >= self.last_acc, "accumulator went backwards after {op:?}");
        self.last_acc = p.acc_reward_per_share;
        // 2. lamports are conserved: everything received is either paid, parked for the running
        //    window, waiting for a first active set, or attributed and not yet claimed (incl. dust)
        assert_eq!(
            self.received,
            self.claimed + p.carry + p.pending_rewards + p.unclaimed_rewards,
            "lamport accounting broke after {op:?}"
        );
        // 3. never promise (much) more than was attributed: the per-position view can
        //    round up by one lamport each; `claim` caps the actual payout at `unclaimed`
        let owed_now: u128 = self.pos.iter().map(|x| p.pending_rewards_of(x) as u128).sum();
        assert!(owed_now <= p.unclaimed_rewards as u128 + self.pos.len() as u128, "owed {owed_now} > unclaimed {} after {op:?}", p.unclaimed_rewards);
        // 4. totals match the positions (after activation of what is activatable)
        let mut act = 0u64;
        let mut pend = 0u64;
        for x in &self.pos {
            let mut q = *x;
            p.activate(&mut q).unwrap();
            act += q.amount;
            pend += q.pending;
        }
        let mut pool_view = *p;
        // what the pool would hold once pending deposits of past epochs are activated
        let _ = pool_view.sync(self.now);
        assert_eq!(act + pend, p.total_active + p.total_pending, "token totals drifted after {op:?}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, max_shrink_iters: 2000, ..ProptestConfig::default() })]

    #[test]
    fn random_sequences_keep_every_invariant(ops in proptest::collection::vec(op(), 1..200)) {
        let mut w = World::new();
        for o in &ops { w.apply(o); }
        // settle everything at the end: all claims together never exceed what was attributed
        w.now += E; w.pool.sync(w.now).unwrap();
        let mut total = 0u128;
        for i in 0..4 { total += w.pool.claim(&mut w.pos[i]).unwrap() as u128; }
        prop_assert!(total + w.claimed as u128 <= w.received as u128);
    }

    /// Two believers active for a whole window split its fees pro rata, to the lamport.
    #[test]
    fn whole_window_split_is_pro_rata(a in 1..=SUPPLY / 2, b in 1..=SUPPLY / 2, fees in 1..=1_000 * SOL) {
        let mut pool = Pool::new(0, E).unwrap();
        let (mut pa, mut pb) = (Position::default(), Position::default());
        pool.stake(&mut pa, a).unwrap();
        pool.stake(&mut pb, b).unwrap();
        pool.sync(E).unwrap(); // both active for epoch 1
        pool.receive(fees).unwrap();
        pool.sync(2 * E).unwrap();
        let ra = pool.claim(&mut pa).unwrap() as u128;
        let rb = pool.claim(&mut pb).unwrap() as u128;
        let total = (a as u128) + (b as u128);
        let exact_a = (fees as u128) * (a as u128) / total;
        let exact_b = (fees as u128) * (b as u128) / total;
        prop_assert!(ra <= exact_a && exact_a - ra <= 1, "a: got {ra}, exact {exact_a}");
        prop_assert!(rb <= exact_b && exact_b - rb <= 1, "b: got {rb}, exact {exact_b}");
        prop_assert!(ra + rb <= fees as u128);
    }

    /// Griefing attempt: a 1-base-unit staker alone in the pool inflates the
    /// accumulator with a large (refunded-to-himself) fee, then a whale stakes
    /// the whole supply. Nothing may overflow: the whale must be able to
    /// settle, claim and leave.
    #[test]
    fn tiny_staker_then_whale_never_overflows(grief in 1..=100_000 * SOL, whale in 1..=SUPPLY, windows in 1..50u64) {
        let mut pool = Pool::new(0, E).unwrap();
        let mut tiny = Position::default();
        pool.stake(&mut tiny, 1).unwrap();
        pool.sync(E).unwrap();
        for w in 1..=windows {
            pool.receive(grief).unwrap();
            pool.sync((w as i64 + 1) * E).unwrap();
        }
        let _ = pool.claim(&mut tiny).unwrap();
        let mut big = Position::default();
        let t = (windows as i64 + 2) * E;
        pool.sync(t).unwrap();
        pool.stake(&mut big, whale).unwrap();
        pool.sync(t + E).unwrap();
        pool.receive(SOL).unwrap();
        pool.sync(t + 2 * E).unwrap();
        let owed = pool.claim(&mut big).unwrap();
        prop_assert!(owed <= SOL);
        pool.request_unstake(&mut big, whale, t + 2 * E, E).unwrap();
        prop_assert_eq!(pool.unstake(&mut big, t + 3 * E).unwrap(), whale);
        let _ = PRECISION;
    }
}
