use anchor_lang::prelude::*;

#[event]
pub struct PoolCreated {
    pub mint: Pubkey,
    pub pool: Pubkey,
    pub creator: Pubkey,
    /// pump.fun creator is this program's PDA: split locked, parameters forced to defaults.
    pub locked: bool,
    pub epoch_seconds: i64,
    pub cooldown_seconds: i64,
    pub timestamp: i64,
}

/// Emitted when a window boundary is crossed and its fees are attributed.
#[event]
pub struct EpochClosed {
    pub mint: Pubkey,
    pub epoch_index: u64,
    pub attributed: u64,
    pub total_active: u64,
    pub acc_reward_per_share: u128,
    pub timestamp: i64,
}

/// A deposit attributed by the attestor. `origin` is the signature of the
/// transfer into the vault: anyone can open it and check that `owner` is the
/// sender.
#[event]
pub struct DepositCredited {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub origin: [u8; 64],
    pub active_from_epoch: u64,
    pub total_pending: u64,
    pub timestamp: i64,
}

#[event]
pub struct ConfigChanged {
    pub admin: Pubkey,
    pub attestor: Pubkey,
    pub fee_receiver: Pubkey,
    pub launch_fee: u64,
    pub exit_fee: u64,
    pub timestamp: i64,
}

#[event]
pub struct Activated {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub timestamp: i64,
}

/// An exit: signed by the owner (`origin` is all zeros) or attributed by the
/// attestor (`origin` is the signature of the SOL transfer from `owner` to the
/// pool address). `fee` is what the platform received for it.
#[event]
pub struct ExitRequested {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub origin: [u8; 64],
    pub fee: u64,
    pub cooldown_until: i64,
    pub total_active: u64,
    pub timestamp: i64,
}

#[event]
pub struct Unstaked {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub timestamp: i64,
}

#[event]
pub struct Claimed {
    pub mint: Pubkey,
    pub owner: Pubkey,
    /// Paid to the owner.
    pub lamports: u64,
    /// Kept by the caller for the network fee (≤ MAX_CLAIM_FEE; 0 when the owner calls).
    pub fee: u64,
    pub timestamp: i64,
}

/// An empty position closed; its lifetime payouts are recorded here for good.
#[event]
pub struct PositionClosed {
    pub mint: Pubkey,
    pub owner: Pubkey,
    /// Everything ever paid to the owner by this position, last payout included.
    pub total_claimed: u64,
    pub last_payout: u64,
    pub timestamp: i64,
}

#[event]
pub struct FeeSplitLocked {
    pub mint: Pubkey,
    pub pool: Pubkey,
    /// Share of the creator fees locked to the pool of stakers: always 10 000.
    pub pool_bps: u16,
    pub timestamp: i64,
}

/// A stake signed by its owner (wallet path).
#[event]
pub struct Staked {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub active_from_epoch: u64,
    pub total_pending: u64,
    pub timestamp: i64,
}

/// A token that was not the pool's coin, sent to the pool address by mistake, sent back by the admin.
#[event]
pub struct StrayTokenReturned {
    pub pool: Pubkey,
    pub mint: Pubkey,
    pub to: Pubkey,
    pub amount: u64,
    pub timestamp: i64,
}
