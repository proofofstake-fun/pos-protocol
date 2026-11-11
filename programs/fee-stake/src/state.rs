use anchor_lang::prelude::*;
use stake_math::{Pool as PoolMath, Position as PositionMath, ACC_RING};

/// One pool per coin. PDA `["pool", mint]`.
///
/// Registered as a locked shareholder (80 to 100 %) in the coin's pump.fun fee
/// sharing config: `distribute_creator_fees_v2` (permissionless) deposits SOL
/// straight onto this account. Every instruction first runs `sync`, which
/// parks newly received lamports for the running window and, at a window
/// boundary, attributes the finished window's fees to the stakers that were
/// active for the whole window.
#[account]
#[derive(InitSpace)]
pub struct StakePool {
    pub mint: Pubkey,
    pub creator: Pubkey,
    pub token_vault: Pubkey,

    pub epoch_seconds: i64,
    pub epoch_index: u64,
    pub total_active: u64,
    pub total_pending: u64,
    pub acc_reward_per_share: u128,
    pub acc_ring: [u128; ACC_RING],
    pub carry: u64,
    pub pending_rewards: u64,
    pub unclaimed_rewards: u64,
    pub total_distributed: u64,
    /// Lamports already known to the pool (rent + unclaimed + carry).
    pub accounted_lamports: u64,
    /// Tokens attributed to positions and still in the vault (active + pending
    /// + cooling down). `credit_deposit` may never push it past the vault's
    /// real balance: the attestor cannot invent tokens.
    pub vault_accounted: u64,

    pub cooldown_seconds: i64,
    pub staker_count: u32,
    pub created_at: i64,
    pub last_sync_at: i64,
    pub bump: u8,
}

impl StakePool {
    pub fn to_math(&self) -> PoolMath {
        PoolMath {
            epoch_seconds: self.epoch_seconds,
            epoch_index: self.epoch_index,
            total_active: self.total_active,
            total_pending: self.total_pending,
            acc_reward_per_share: self.acc_reward_per_share,
            acc_ring: self.acc_ring,
            carry: self.carry,
            pending_rewards: self.pending_rewards,
            unclaimed_rewards: self.unclaimed_rewards,
            total_distributed: self.total_distributed,
        }
    }

    pub fn from_math(&mut self, m: PoolMath) {
        self.epoch_index = m.epoch_index;
        self.total_active = m.total_active;
        self.total_pending = m.total_pending;
        self.acc_reward_per_share = m.acc_reward_per_share;
        self.acc_ring = m.acc_ring;
        self.carry = m.carry;
        self.pending_rewards = m.pending_rewards;
        self.unclaimed_rewards = m.unclaimed_rewards;
        self.total_distributed = m.total_distributed;
    }
}

/// One per (owner, pool). PDA `["position", pool, owner]`.
///
/// `owner` is the wallet that staked: it signed `stake`, or it sent tokens to
/// the pool address and the attestor attested it in `credit_deposit`. Rewards can be pushed to it by anyone
/// through `claim`, and tokens only ever go back to `owner`.
#[account]
#[derive(InitSpace)]
pub struct StakePosition {
    pub pool: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub pending: u64,
    pub pending_epoch: u64,
    pub reward_debt: u128,
    pub rewards_owed: u64,
    pub cooldown_amount: u64,
    pub cooldown_until: i64,
    pub total_claimed: u64,
    /// Who paid this account's rent and gets it back at `close_position`: the
    /// owner (wallet stake) or the attestor (deposit credited from a transfer).
    pub rent_payer: Pubkey,
    pub bump: u8,
}

/// `["config"]`: who may attest deposits and exits, where the platform's fees
/// go, and who may change that.
#[account]
#[derive(InitSpace)]
pub struct Config {
    /// Multisig on mainnet. Can rotate `attestor`, set the fees and hand over `admin`.
    pub admin: Pubkey,
    /// The crank's attribution key: the only signer `credit_deposit` and
    /// `request_exit` accept.
    pub attestor: Pubkey,
    /// The platform's wallet: receives the launch fee and the exit fees. It
    /// never receives any part of a coin's trading fees.
    pub fee_receiver: Pubkey,
    /// Paid once, by whoever opens a coin's pool (≤ MAX_LAUNCH_FEE).
    pub launch_fee: u64,
    /// Paid by a staker asking to leave (≤ MAX_EXIT_FEE).
    pub exit_fee: u64,
    pub bump: u8,
}

impl StakePosition {
    pub fn to_math(&self) -> PositionMath {
        PositionMath {
            amount: self.amount,
            pending: self.pending,
            pending_epoch: self.pending_epoch,
            reward_debt: self.reward_debt,
            rewards_owed: self.rewards_owed,
            cooldown_amount: self.cooldown_amount,
            cooldown_until: self.cooldown_until,
        }
    }

    pub fn from_math(&mut self, m: PositionMath) {
        self.amount = m.amount;
        self.pending = m.pending;
        self.pending_epoch = m.pending_epoch;
        self.reward_debt = m.reward_debt;
        self.rewards_owed = m.rewards_owed;
        self.cooldown_amount = m.cooldown_amount;
        self.cooldown_until = m.cooldown_until;
    }
}
