//! Prints random histories with the exact state after every step, as JSON.
//! `sdk/tests/stake-mirror.test.ts` replays them with the TypeScript copy of
//! this crate (the one the crank and the site use to decide and display) and
//! requires every number to match to the lamport.
//!
//!   cargo run -q --release -p stake-math --example histories [count] [seed]

use stake_math::{Pool, Position, DEFAULT_COOLDOWN_SECONDS, DEFAULT_EPOCH_SECONDS};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 { self.next() % n }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let count: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(200);
    let mut rng = Rng(args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0x9E37_79B9_7F4A_7C15));
    const USERS: usize = 5;
    let mut out = String::from("[");
    for h in 0..count {
        let mut now: i64 = 1_800_000_000 + rng.below(60) as i64;
        let mut pool = Pool::new(now, DEFAULT_EPOCH_SECONDS).unwrap();
        let mut pos = vec![Position::default(); USERS];
        if h > 0 { out.push(','); }
        out.push_str(&format!("{{\"start\":{now},\"steps\":["));
        let n = 20 + rng.below(100) as usize;
        for i in 0..n {
            let u = rng.below(USERS as u64) as usize;
            let (kind, arg): (&str, u64) = match rng.below(12) {
                0..=2 => { let long = rng.below(10) == 0; let s = 1 + rng.below(if long { 2_000 } else { 150 }); now += s as i64; ("wait", s) }
                3..=4 => { let a = 1_000_000_000 + rng.below(1 << 40); pool.sync(now).unwrap(); pool.stake(&mut pos[u], a).unwrap(); ("stake", a) }
                5..=7 => { let l = 1 + rng.below(3_000_000_000); pool.receive(l).unwrap(); pool.sync(now).unwrap(); ("fees", l) }
                8 => { pool.sync(now).unwrap(); let mut q = pos[u]; pool.activate(&mut q).unwrap(); let t = Pool::staked_total(&q); if t > 0 { pool.request_unstake(&mut pos[u], t, now, DEFAULT_COOLDOWN_SECONDS).unwrap(); } ("exit", t) }
                9 => { pool.sync(now).unwrap(); let got = pool.claim(&mut pos[u]).unwrap(); ("claim", got) }
                10 => { pool.sync(now).unwrap(); let got = pool.unstake(&mut pos[u], now).unwrap_or(0); ("unstake", got) }
                _ => { pool.sync(now).unwrap(); pool.activate(&mut pos[u]).unwrap(); ("activate", 0) }
            };
            if i > 0 { out.push(','); }
            let owed: Vec<String> = pos.iter().map(|p| format!("\"{}\"", pool.pending_rewards_of(p))).collect();
            out.push_str(&format!(
                "{{\"op\":\"{kind}\",\"user\":{u},\"arg\":\"{arg}\",\"now\":{now},\"epoch\":{},\"active\":\"{}\",\"pending\":\"{}\",\"acc\":\"{}\",\"carry\":\"{}\",\"waiting\":\"{}\",\"unclaimed\":\"{}\",\"distributed\":\"{}\",\"owed\":[{}]}}",
                pool.epoch_index, pool.total_active, pool.total_pending, pool.acc_reward_per_share, pool.carry, pool.pending_rewards, pool.unclaimed_rewards, pool.total_distributed, owed.join(",")
            ));
        }
        out.push_str("]}");
    }
    out.push(']');
    println!("{out}");
}
