//! # fee-stake — the program behind Proof of Stake (PoS)
//!
//! Stake a pump.fun coin, earn its creator fees pro-rata, in 1-minute windows.
//!
//! Fee flow (no CPI into pump.fun needed):
//! ```text
//! create_v2 (pump)                  creator = ["creator", mint] (this program's PDA)
//! init_pool (this program)          POOL = PDA ["pool", mint]; pays the platform's launch fee
//! lock_fee_split (this program)     pump.fun split = {POOL: 100 %}  ← locked forever
//!      …trading…
//! distribute_creator_fees_v2 (fees, permissionless, every minute) → SOL lands on POOL
//! any instruction here → sync(): fees of the finished window are attributed to
//!                        the positions that were active for the whole window
//! ```
//!
//! Two ways in, two ways out, the same position either way:
//! * with a wallet: `stake` moves the coins and credits the signer;
//!   `request_unstake` (signed, pays the exit fee) starts the 1-hour cooldown;
//! * with nothing to connect: send the coin to the pool address → the attestor
//!   (the crank's key, set in `Config`) reads the sender and calls
//!   `credit_deposit`; send ≥ 0.01 SOL to the pool address from that same
//!   wallet → the attestor calls `request_exit`.
//! After the cooldown anyone can call `unstake`: the tokens only ever go back
//! to the position's owner. `claim` is permissionless too: the crank pushes SOL
//! to every owner each window, and an owner can always pull its own.
//! An owner therefore never depends on the crank to get its coins or SOL back.
//!
//! Trust model for the transfer path: the chain cannot tell this program who
//! sent a plain token transfer, so attribution is attested off-chain. The
//! program bounds it (never more than the vault holds), restricts it (one key,
//! rotated by the admin) and publishes it (every credit carries its origin
//! signature).
//!
//! The platform: `Config.fee_receiver` gets a launch fee when a pool opens and
//! an exit fee when a staker leaves, both capped in this program. It gets no
//! part of any coin's trading fees. The admin can rotate the attestor, set
//! those two fees within their caps, and send back a token that is not the
//! pool's coin when someone sent it to a pool address by mistake. No pause, no
//! way to touch the stakers' coins or SOL.

#![allow(clippy::result_large_err)]

use anchor_lang::prelude::*;

pub mod errors;
pub mod events;
pub mod instructions;
pub mod state;

use instructions::*;

declare_id!("PoSBsxAZVhLmPAUv9YLcWHauVjzSro6smzt92JPWtLY");

#[program]
pub mod fee_stake {
    use super::*;

    /// Once per deployment: the deployer becomes admin, `attestor` is the crank's
    /// key, `fee_receiver` the platform's wallet for the launch and exit fees.
    pub fn init_config(ctx: Context<InitConfig>, attestor: Pubkey, fee_receiver: Pubkey, launch_fee: u64, exit_fee: u64) -> Result<()> {
        instructions::init_config(ctx, attestor, fee_receiver, launch_fee, exit_fee)
    }

    /// Admin only: the platform's wallet and its two fees, within the caps.
    pub fn set_fees(ctx: Context<AdminOnly>, fee_receiver: Pubkey, launch_fee: u64, exit_fee: u64) -> Result<()> {
        instructions::set_fees(ctx, fee_receiver, launch_fee, exit_fee)
    }

    /// Wallet path in: the signer's coins go to the vault and are credited at once.
    pub fn stake(ctx: Context<Stake>, amount: u64) -> Result<()> {
        instructions::stake(ctx, amount)
    }

    /// Wallet path out: the signer pays the exit fee; `amount` (u64::MAX =
    /// everything) goes into cooldown.
    pub fn request_unstake(ctx: Context<RequestUnstake>, amount: u64) -> Result<()> {
        instructions::request_unstake(ctx, amount)
    }

    /// Admin only: rotate the attribution key.
    pub fn set_attestor(ctx: Context<AdminOnly>, attestor: Pubkey) -> Result<()> {
        instructions::set_attestor(ctx, attestor)
    }

    /// Admin only: hand the admin role over (to the multisig).
    pub fn set_admin(ctx: Context<AdminOnly>, admin: Pubkey) -> Result<()> {
        instructions::set_admin(ctx, admin)
    }

    /// Create the pool (pays the launch fee): 1-minute windows, 1-hour leaving delay, always. The two
    /// arguments are kept for compatibility and ignored (the test build still
    /// reads them for legacy pools; see `MIN_COOLDOWN`).
    pub fn init_pool(ctx: Context<InitPool>, epoch_seconds: Option<i64>, cooldown_seconds: Option<i64>) -> Result<()> {
        instructions::init_pool(ctx, epoch_seconds, cooldown_seconds)
    }

    /// Permissionless: park received fees and cross any window boundary.
    pub fn harvest(ctx: Context<Harvest>) -> Result<()> {
        instructions::harvest(ctx)
    }

    /// Attestor only: credit `owner` for `amount` sent to the pool address in
    /// transaction `origin`. Bounded by the vault's real balance.
    pub fn credit_deposit(ctx: Context<CreditDeposit>, amount: u64, origin: [u8; 64]) -> Result<()> {
        instructions::credit_deposit(ctx, amount, origin)
    }

    /// Attestor only: `owner` sent ≥ 0.01 SOL to the pool address in `origin`;
    /// everything they have goes into cooldown.
    pub fn request_exit(ctx: Context<RequestExit>, origin: [u8; 64]) -> Result<()> {
        instructions::request_exit(ctx, origin)
    }

    /// Permissionless: settle a pending deposit once its window has passed.
    pub fn activate(ctx: Context<Activate>) -> Result<()> {
        instructions::activate(ctx)
    }

    /// Permissionless: push owed SOL to `owner`.
    pub fn claim(ctx: Context<Claim>, fee: u64) -> Result<()> {
        instructions::claim(ctx, fee)
    }

    /// Permissionless: close an empty position, pay its owner what is left,
    /// return the rent to the attestor.
    pub fn close_position(ctx: Context<ClosePosition>) -> Result<()> {
        instructions::close_position(ctx)
    }

    /// Permissionless: return cooled-down tokens to `owner`.
    pub fn unstake(ctx: Context<Unstake>) -> Result<()> {
        instructions::unstake(ctx)
    }

    /// Lock the coin's pump.fun creator-fee split to 100 % to the pool, for a coin whose pump.fun `creator` is this program's PDA. The pool PDA signs
    /// the fee-program CPIs, so nobody (creator or platform) can change the
    /// split afterwards. See `instructions::lock_fee_split`.
    pub fn lock_fee_split<'info>(ctx: Context<'_, '_, 'info, 'info, LockFeeSplit<'info>>, create_accounts: u8, create_data: Vec<u8>, update_data: Vec<u8>, fund_lamports: u64) -> Result<()> {
        instructions::lock_fee_split(ctx, create_accounts, create_data, update_data, fund_lamports)
    }

    /// Admin only: send back, to `to`, a token that is NOT the pool's coin and
    /// was sent to the pool address by mistake. The stakers' coin and the
    /// pool's SOL can never leave this way.
    pub fn rescue_token(ctx: Context<RescueToken>) -> Result<()> {
        instructions::rescue_token(ctx)
    }
}
