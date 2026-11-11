//! Model check: random histories against a reference written from the rules,
//! not from the code.
//!
//! The rules, as promised to believers:
//!   * time is cut into 1-minute windows aligned on the clock;
//!   * fees that land during window W (collected at its end, in the same
//!     transaction as the boundary) are split pro rata between the tokens that
//!     were active for the WHOLE of W;
//!   * a deposit made during W starts earning at W+1;
//!   * asking to leave stops earning at once (no share of the running window)
//!     and the tokens can be taken back one hour later;
//!   * fees that land while nobody is active wait for the first active set.
//!
//! The reference keeps exact per-window shares (u128 division, floored per
//! window). The accumulator in `stake-math` is allowed to differ from it only
//! by rounding: at most one lamport per window per position, and never in the
//! pool's favour of paying out more than it received.

use proptest::prelude::*;
use stake_math::{Pool, Position, DEFAULT_COOLDOWN_SECONDS, DEFAULT_EPOCH_SECONDS};

const E: i64 = DEFAULT_EPOCH_SECONDS;
const USERS: usize = 6;
const T: u64 = 1_000_000; // one token (6 decimals)

#[derive(Debug, Clone)]
enum Op {
    /// advance the clock by this many seconds (crosses 0..n windows)
    Wait(i64),
    Deposit(usize, u64),
    /// lamports collected (pump.fun distribute + harvest, or a 0.01 SOL exit, a stray transfer)
    Fees(u64),
    Exit(usize),
    Claim(usize),
    Return(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (1i64..200).prop_map(Op::Wait),
        3 => (0..USERS, 1_000u64..50_000_000).prop_map(|(u, a)| Op::Deposit(u, a * T / 1_000)),
        4 => (1u64..2_000_000_000).prop_map(Op::Fees),
        1 => (0..USERS).prop_map(Op::Exit),
        2 => (0..USERS).prop_map(Op::Claim),
        1 => (0..USERS).prop_map(Op::Return),
    ]
}

// ------------------------------------------------------------------ reference --

#[derive(Default, Clone)]
struct RefUser {
    active: u64,
    pending: u64,
    leaving: u64,
    leaving_until: i64,
    /// sum over windows of floor(fees × stake / total)
    earned: u128,
    /// number of windows this user shared in (rounding budget)
    windows: u64,
    paid: u128,
}

struct Reference {
    epoch: i64,
    carry: u128,
    waiting: u128,
    users: Vec<RefUser>,
    received: u128,
}

impl Reference {
    fn new(now: i64) -> Self {
        Self { epoch: now / E, carry: 0, waiting: 0, users: vec![RefUser::default(); USERS], received: 0 }
    }
    /// The window(s) up to `now` are over: share what landed, then deposits of the old window join.
    fn cross(&mut self, now: i64) {
        let e = now / E;
        if e <= self.epoch {
            return;
        }
        let pot = self.carry + self.waiting;
        let total: u128 = self.users.iter().map(|u| u.active as u128).sum();
        if pot > 0 {
            if total == 0 {
                self.waiting = pot;
            } else {
                for u in self.users.iter_mut().filter(|u| u.active > 0) {
                    u.earned += pot * u.active as u128 / total;
                    u.windows += 1;
                }
                self.waiting = 0;
            }
            self.carry = 0;
        }
        for u in self.users.iter_mut() {
            u.active += u.pending;
            u.pending = 0;
        }
        self.epoch = e;
    }
}

// ---------------------------------------------------------------- the checks --

fn run(ops: &[Op], crank_activates: bool) -> Result<(), TestCaseError> {
    let mut now: i64 = 1_800_000_000; // some n:00
    let mut pool = Pool::new(now, E).unwrap();
    let mut pos = vec![Position::default(); USERS];
    let mut r = Reference::new(now);
    let mut paid_by_pool: u128 = 0;

    for o in ops {
        match *o {
            Op::Wait(s) => {
                now += s;
                // the crank closes every window that has something to do; nothing else happens here
            }
            Op::Fees(l) => {
                // same order as the program's `sync`: what landed is parked, then the boundary is crossed
                pool.receive(l).unwrap();
                pool.sync(now).unwrap();
                r.carry += l as u128;
                r.received += l as u128;
                r.cross(now);
            }
            Op::Deposit(u, a) => {
                pool.sync(now).unwrap();
                r.cross(now);
                pool.stake(&mut pos[u], a).unwrap();
                r.users[u].pending += a;
            }
            Op::Exit(u) => {
                pool.sync(now).unwrap();
                r.cross(now);
                let mut q = pos[u];
                pool.activate(&mut q).unwrap();
                let total = Pool::staked_total(&q);
                if total == 0 {
                    continue;
                }
                pool.request_unstake(&mut pos[u], total, now, DEFAULT_COOLDOWN_SECONDS).unwrap();
                let ru = &mut r.users[u];
                ru.leaving += ru.active + ru.pending;
                ru.active = 0;
                ru.pending = 0;
                ru.leaving_until = now + DEFAULT_COOLDOWN_SECONDS;
            }
            Op::Claim(u) => {
                pool.sync(now).unwrap();
                r.cross(now);
                let got = pool.claim(&mut pos[u]).unwrap() as u128;
                paid_by_pool += got;
                r.users[u].paid += got;
            }
            Op::Return(u) => {
                pool.sync(now).unwrap();
                r.cross(now);
                let early = now < r.users[u].leaving_until;
                match pool.unstake(&mut pos[u], now) {
                    Ok(out) => {
                        prop_assert!(!early, "tokens returned before the hour");
                        prop_assert_eq!(out, r.users[u].leaving, "returned amount");
                        r.users[u].leaving = 0;
                    }
                    Err(_) => prop_assert!(early || r.users[u].leaving == 0, "return refused after the hour"),
                }
            }
        }
        if crank_activates {
            // what the crank does after each window close
            for p in pos.iter_mut() {
                pool.activate(p).unwrap();
            }
        }
        // never pays out more than it received
        prop_assert!(paid_by_pool <= r.received, "paid {} > received {}", paid_by_pool, r.received);
        // the active set is the reference's
        let ref_active: u64 = r.users.iter().map(|u| u.active).sum();
        prop_assert_eq!(pool.total_active, ref_active, "active tokens");
    }

    // end: one more window so everything that landed is shared, then everyone is paid
    now = (now / E + 2) * E;
    pool.sync(now).unwrap();
    r.cross(now);
    let mut total_paid = paid_by_pool;
    for u in 0..USERS {
        let got = pool.claim(&mut pos[u]).unwrap() as u128;
        total_paid += got;
        let ru = &mut r.users[u];
        ru.paid += got;
        let slack = ru.windows as u128 + 1;
        if crank_activates {
            // exact up to rounding, both ways
            prop_assert!(ru.paid + slack >= ru.earned, "user {u}: paid {} < due {} (−{})", ru.paid, ru.earned, ru.earned - ru.paid);
        }
        prop_assert!(ru.paid <= ru.earned + slack, "user {u}: paid {} > due {}", ru.paid, ru.earned);
    }
    prop_assert!(total_paid <= r.received, "total paid {} > received {}", total_paid, r.received);
    // what was received is paid, waiting for an active set, or rounding dust
    let dust = r.received - total_paid - r.waiting;
    let windows: u128 = r.users.iter().map(|u| u.windows as u128).sum();
    if crank_activates {
        prop_assert!(dust <= windows + USERS as u128, "{} lamports unaccounted for", dust);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 3000, ..ProptestConfig::default() })]

    /// The crank activates pending deposits after every close (as it does):
    /// every payout equals what the rules say, to the lamport per window.
    #[test]
    fn payouts_follow_the_rules(ops in proptest::collection::vec(op(), 1..120)) {
        run(&ops, true)?;
    }

    /// Nobody activates anything (crank down for a long time): positions may
    /// earn less, never more, and the pool never pays out more than it received.
    #[test]
    fn without_the_crank_nobody_is_overpaid(ops in proptest::collection::vec(op(), 1..120)) {
        run(&ops, false)?;
    }
}

/// The deposit window earns nothing, the next one does, leaving stops at once.
#[test]
fn the_three_rules_by_hand() {
    let t0 = 1_800_000_000; // n:00
    let mut pool = Pool::new(t0, E).unwrap();
    let (mut a, mut b) = (Position::default(), Position::default());
    pool.sync(t0 + 10).unwrap();
    pool.stake(&mut a, 100 * T).unwrap(); // n:00:10
    pool.receive(1_000).unwrap();
    pool.sync(t0 + 50).unwrap(); // fees during a's deposit window
    pool.sync(t0 + 62).unwrap(); // window closed: nobody was active → the 1,000 wait
    pool.activate(&mut a).unwrap();
    pool.sync(t0 + 70).unwrap();
    pool.stake(&mut b, 100 * T).unwrap(); // b deposits during n:01
    pool.receive(2_000).unwrap();
    pool.sync(t0 + 121).unwrap(); // n:01 closed: a alone was active → 1,000 waiting + 2,000
    pool.activate(&mut b).unwrap();
    assert_eq!(pool.claim(&mut a).unwrap(), 3_000);
    assert_eq!(pool.claim(&mut b).unwrap(), 0);
    pool.receive(4_000).unwrap();
    pool.request_unstake(&mut a, 100 * T, t0 + 130, DEFAULT_COOLDOWN_SECONDS).unwrap(); // a leaves during n:02
    pool.sync(t0 + 181).unwrap(); // n:02 closed: only b
    assert_eq!(pool.claim(&mut b).unwrap(), 4_000);
    assert_eq!(pool.claim(&mut a).unwrap(), 0);
    assert!(pool.unstake(&mut a, t0 + 130 + 3_599).is_err());
    assert_eq!(pool.unstake(&mut a, t0 + 130 + 3_600).unwrap(), 100 * T);
}
