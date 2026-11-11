use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::{
    associated_token::AssociatedToken,
    token_2022::spl_token_2022::{
        extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions},
        state::Mint as Mint2022,
    },
    token_interface::{self, Mint, TokenAccount, TokenInterface, TransferChecked},
};
use stake_math::{Pool as PoolMath, DEFAULT_COOLDOWN_SECONDS, DEFAULT_EPOCH_SECONDS};

use crate::errors::{lift, FeeStakeError};
use crate::events::*;
use crate::state::{Config, StakePool, StakePosition};

pub const POOL_SEED: &[u8] = b"pool";
pub const POSITION_SEED: &[u8] = b"position";
/// `["config"]`: one account per deployment holding the admin (a multisig on
/// mainnet) and the attestor (the crank's attribution key).
pub const CONFIG_SEED: &[u8] = b"config";

/// Smallest lamport transfer to a pool address the crank reads as "the sender
/// wants out": 0.01 SOL. Documented for the crank and the UI; the program
/// itself treats every lamport that reaches a pool as fees for the stakers
/// (`request_exit` moves the platform's exit fee out of it when it is still in
/// the running window).
#[constant]
pub const MIN_EXIT_LAMPORTS: u64 = 10_000_000;

/// pump.fun bonding-curve program: a pool may only be opened for a coin that
/// lives there, by that coin's creator (`BondingCurve.creator`).
pub const PUMP_PROGRAM_ID: Pubkey = pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
pub const BONDING_CURVE_SEED: &[u8] = b"bonding-curve";
/// `["creator", mint]`: the pump.fun *creator* of a locked coin. A system-owned
/// PDA of this program with no private key: it signs the fee-program calls
/// in `lock_fee_split` and nothing else, so the split cannot be changed again.
/// (The pool account itself cannot play this role: an account owned by this
/// program cannot pay rent inside another program's CPI.)
pub const CREATOR_SEED: &[u8] = b"creator";
/// pump.fun fee-sharing program (`create_fee_sharing_config`, `update_fee_shares_v2`).
pub const PUMP_FEE_PROGRAM_ID: Pubkey = pubkey!("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
/// `["sharing-config", mint]` under the fee program: once a coin has a fee
/// sharing config, pump.fun makes that config the coin's creator.
pub const SHARING_CONFIG_SEED: &[u8] = b"sharing-config";
const CREATE_FEE_SHARING_CONFIG_DISC: [u8; 8] = [195, 78, 86, 76, 111, 52, 251, 213];
const UPDATE_FEE_SHARES_V2_DISC: [u8; 8] = [111, 251, 49, 6, 78, 78, 106, 18];
/// Caps on what the admin can set as platform fees: the launch fee paid once
/// when a pool opens, and the exit fee paid when a staker asks to leave.
#[constant]
pub const MAX_LAUNCH_FEE: u64 = 200_000_000; // 0.2 SOL
#[constant]
pub const MAX_EXIT_FEE: u64 = 50_000_000; // 0.05 SOL
/// `BondingCurve` layout: 8 discriminator + 5 × u64 + `complete: bool` → `creator` at 49.
const BONDING_CURVE_CREATOR_OFFSET: usize = 8 + 5 * 8 + 1;
/// Smallest deposit the attestor may credit: 1,000 tokens (6 decimals). The
/// attestor pays the rent of new positions, so dust must not be able to create
/// them. The crank adds a value floor on top (off-chain).
#[constant]
pub const MIN_CREDIT_AMOUNT: u64 = 1_000 * 1_000_000;

/// Every pool pays every minute and returns the coins one hour after the
/// 0.01 SOL: `init_pool` always writes DEFAULT_EPOCH_SECONDS (60) and
/// DEFAULT_COOLDOWN_SECONDS (3 600), whoever opens it and whatever it passes.
/// The bounds below only matter in the test build, which still honours the
/// arguments of a legacy (wallet-created) pool so the suite can use a 60 s
/// leaving delay and run in minutes.
pub const MIN_EPOCH: i64 = 60;
pub const MAX_EPOCH: i64 = 3600;
#[cfg(not(feature = "skip-curve-check"))]
pub const MIN_COOLDOWN: i64 = 5 * 60;
#[cfg(feature = "skip-curve-check")]
pub const MIN_COOLDOWN: i64 = 60;
pub const MAX_COOLDOWN: i64 = 24 * 3600;

// ---------------------------------------------------------------- helpers --

/// Park newly received lamports and cross any window boundary.
fn sync(pool: &mut Account<StakePool>, now: i64) -> Result<()> {
    let lamports = pool.to_account_info().lamports();
    let received = lamports.saturating_sub(pool.accounted_lamports);
    let mut m = pool.to_math();
    if received > 0 {
        lift(m.receive(received))?;
        pool.accounted_lamports = lamports;
    }
    let before = m.epoch_index;
    let crossed = lift(m.sync(now))?;
    pool.from_math(m);
    pool.last_sync_at = now;
    if crossed {
        emit!(EpochClosed {
            mint: pool.mint,
            epoch_index: before,
            attributed: pool.total_distributed,
            total_active: pool.total_active,
            acc_reward_per_share: pool.acc_reward_per_share,
            timestamp: now,
        });
    }
    Ok(())
}

fn check_fees(fee_receiver: &Pubkey, launch_fee: u64, exit_fee: u64) -> Result<()> {
    require!(*fee_receiver != Pubkey::default() && *fee_receiver != crate::ID, FeeStakeError::BadFeeReceiver);
    require!(launch_fee <= MAX_LAUNCH_FEE && exit_fee <= MAX_EXIT_FEE, FeeStakeError::FeeTooHigh);
    Ok(())
}

fn pay_from_pool<'info>(pool: &AccountInfo<'info>, to: &AccountInfo<'info>, lamports: u64) -> Result<()> {
    if lamports == 0 {
        return Ok(());
    }
    let rent_min = Rent::get()?.minimum_balance(pool.data_len());
    let remaining = pool.lamports().checked_sub(lamports).ok_or(FeeStakeError::MathOverflow)?;
    require!(remaining >= rent_min, FeeStakeError::RentViolation);
    **pool.try_borrow_mut_lamports()? = remaining;
    **to.try_borrow_mut_lamports()? = to.lamports().checked_add(lamports).ok_or(FeeStakeError::MathOverflow)?;
    Ok(())
}

fn init_position_if_new(pool: &mut Account<StakePool>, pos: &mut Account<StakePosition>, owner: Pubkey, rent_payer: Pubkey, bump: u8) {
    if pos.owner == Pubkey::default() {
        pos.pool = pool.key();
        pos.owner = owner;
        pos.rent_payer = rent_payer;
        pos.bump = bump;
        pool.staker_count = pool.staker_count.saturating_add(1);
    }
}


// --------------------------------------------------------------- init_pool --

#[derive(Accounts)]
pub struct InitPool<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,
    pub mint: InterfaceAccount<'info, Mint>,
    /// CHECK: pump.fun's `BondingCurve` PDA for this mint (seeds + owner verified below).
    /// Its `creator` field must be this program's creator PDA (coin launched by the site),
    /// the signer (a wallet-created coin) or the coin's fee sharing config: only real
    /// pump.fun coins get a pool, and junk mints cannot be used to spam pools.
    #[cfg_attr(not(feature = "skip-curve-check"), account(seeds = [BONDING_CURVE_SEED, mint.key().as_ref()], bump, seeds::program = PUMP_PROGRAM_ID, owner = PUMP_PROGRAM_ID))]
    pub bonding_curve: UncheckedAccount<'info>,
    /// CHECK: `["creator", mint]`, see CREATOR_SEED. Only compared against the curve's creator.
    #[account(seeds = [CREATOR_SEED, mint.key().as_ref()], bump)]
    pub creator_authority: UncheckedAccount<'info>,
    #[account(init, payer = creator, space = 8 + StakePool::INIT_SPACE, seeds = [POOL_SEED, mint.key().as_ref()], bump)]
    pub pool: Account<'info, StakePool>,
    #[account(init, payer = creator, associated_token::mint = mint, associated_token::authority = pool, associated_token::token_program = token_program)]
    pub token_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    /// CHECK: the platform's wallet (`config.fee_receiver`); receives the launch fee.
    #[account(mut, address = config.fee_receiver @ FeeStakeError::BadFeeReceiver)]
    pub fee_receiver: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// Token-2022 extensions a pool cannot work with: a transfer fee or hook would
/// make the vault receive less than what is credited (or run foreign code on
/// every stake), a permanent delegate could empty the vault, a pausable or
/// non-transferable mint could trap it. pump.fun mints carry none of these;
/// refusing them here keeps that an on-chain fact rather than an assumption.
fn reject_unsafe_extensions(mint: &AccountInfo) -> Result<()> {
    if *mint.owner != anchor_spl::token_2022::ID {
        return Ok(()); // classic SPL token: no extensions exist
    }
    let data = mint.try_borrow_data()?;
    let state = StateWithExtensions::<Mint2022>::unpack(&data).map_err(|_| FeeStakeError::UnsafeMint)?;
    for ext in state.get_extension_types().map_err(|_| FeeStakeError::UnsafeMint)? {
        let bad = matches!(
            ext,
            ExtensionType::TransferFeeConfig
                | ExtensionType::TransferHook
                | ExtensionType::PermanentDelegate
                | ExtensionType::NonTransferable
                | ExtensionType::DefaultAccountState
                | ExtensionType::ConfidentialTransferMint
                | ExtensionType::ConfidentialTransferFeeConfig
        );
        require!(!bad, FeeStakeError::UnsafeMint);
    }
    Ok(())
}

pub fn init_pool(ctx: Context<InitPool>, epoch_seconds: Option<i64>, cooldown_seconds: Option<i64>) -> Result<()> {
    reject_unsafe_extensions(&ctx.accounts.mint.to_account_info())?;

    // Is this a locked coin (pump.fun creator = our creator PDA) or a legacy
    // one (creator = the signing wallet)? Either way only real pump.fun coins
    // get a pool, and either way the parameters below are fixed: anyone may
    // open a locked coin's pool, so a stranger must not be able to front-run
    // the creator with slower windows or a longer leaving delay.
    #[cfg(not(feature = "skip-curve-check"))]
    let locked = {
        let data = ctx.accounts.bonding_curve.try_borrow_data()?;
        require!(data.len() >= BONDING_CURVE_CREATOR_OFFSET + 32, FeeStakeError::NotACurve);
        let curve_creator = Pubkey::try_from(&data[BONDING_CURVE_CREATOR_OFFSET..BONDING_CURVE_CREATOR_OFFSET + 32]).map_err(|_| FeeStakeError::NotACurve)?;
        // A coin whose fee sharing config already exists has that config as creator
        // (pump.fun moves it there): still a real pump.fun coin, and the parameters
        // below are fixed, so opening its pool gives the opener nothing to choose.
        let (sharing_config, _) = Pubkey::find_program_address(&[SHARING_CONFIG_SEED, ctx.accounts.mint.key().as_ref()], &PUMP_FEE_PROGRAM_ID);
        require!(
            curve_creator == ctx.accounts.creator_authority.key() || curve_creator == ctx.accounts.creator.key() || curve_creator == sharing_config,
            FeeStakeError::NotTheCreator
        );
        curve_creator == ctx.accounts.creator_authority.key()
    };
    // Test build (no pump.fun on the local validator): passing the creator PDA
    // as `bonding_curve` stands for "locked coin".
    #[cfg(feature = "skip-curve-check")]
    let locked = ctx.accounts.bonding_curve.key() == ctx.accounts.creator_authority.key();

    // The windows and the leaving delay are the program's, not the caller's:
    // 1 minute and 1 hour for every pool, so "paid every minute, coins back an
    // hour after the 0.01 SOL" holds for every coin on this program.
    #[cfg(not(feature = "skip-curve-check"))]
    let (epoch, cooldown) = {
        let _ = (epoch_seconds, cooldown_seconds);
        (DEFAULT_EPOCH_SECONDS, DEFAULT_COOLDOWN_SECONDS)
    };
    #[cfg(feature = "skip-curve-check")]
    let (epoch, cooldown) = if locked {
        (DEFAULT_EPOCH_SECONDS, DEFAULT_COOLDOWN_SECONDS)
    } else {
        (epoch_seconds.unwrap_or(DEFAULT_EPOCH_SECONDS), cooldown_seconds.unwrap_or(DEFAULT_COOLDOWN_SECONDS))
    };
    require!((MIN_EPOCH..=MAX_EPOCH).contains(&epoch), FeeStakeError::InvalidEpoch);
    require!((MIN_COOLDOWN..=MAX_COOLDOWN).contains(&cooldown), FeeStakeError::InvalidCooldown);
    let now = Clock::get()?.unix_timestamp;
    let pool = &mut ctx.accounts.pool;
    pool.mint = ctx.accounts.mint.key();
    pool.creator = ctx.accounts.creator.key();
    pool.token_vault = ctx.accounts.token_vault.key();
    pool.from_math(lift(PoolMath::new(now, epoch))?);
    pool.epoch_seconds = epoch;
    pool.cooldown_seconds = cooldown;
    pool.created_at = now;
    pool.last_sync_at = now;
    pool.bump = ctx.bumps.pool;
    // Only the rent is "accounted": SOL that reached the pool address before the pool
    // existed (fees distributed to it early, a transfer) is shared like any other fee.
    pool.accounted_lamports = Rent::get()?.minimum_balance(pool.to_account_info().data_len());
    emit!(PoolCreated { mint: pool.mint, pool: pool.key(), creator: pool.creator, locked, epoch_seconds: epoch, cooldown_seconds: cooldown, timestamp: now });
    // The platform's launch fee, paid once by whoever opens the pool.
    let launch_fee = ctx.accounts.config.launch_fee;
    if launch_fee > 0 {
        system_program::transfer(
            CpiContext::new(ctx.accounts.system_program.to_account_info(), system_program::Transfer { from: ctx.accounts.creator.to_account_info(), to: ctx.accounts.fee_receiver.to_account_info() }),
            launch_fee,
        )?;
    }
    Ok(())
}

// ----------------------------------------------------------------- harvest --

#[derive(Accounts)]
pub struct Harvest<'info> {
    #[account(mut, seeds = [POOL_SEED, pool.mint.as_ref()], bump = pool.bump)]
    pub pool: Account<'info, StakePool>,
}

pub fn harvest(ctx: Context<Harvest>) -> Result<()> {
    sync(&mut ctx.accounts.pool, Clock::get()?.unix_timestamp)
}

// ------------------------------------------------------------------ config --

/// Created once per deployment by the deployer, who becomes `admin` (hand it
/// to the multisig with `set_admin`). `attestor` is the only key allowed to
/// attribute deposits and exits; it holds nothing and can be rotated by admin.
///
/// Only the program's upgrade authority can create it: otherwise anyone watching
/// the deployment could call it first and make themselves admin (and choose the
/// attestor). Run it right after deploying, before handing the upgrade authority
/// to the multisig (scripts/deploy-mainnet.sh does it in that order).
#[derive(Accounts)]
pub struct InitConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(init, payer = admin, space = 8 + Config::INIT_SPACE, seeds = [CONFIG_SEED], bump)]
    pub config: Account<'info, Config>,
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ FeeStakeError::NotUpgradeAuthority)]
    pub program: Program<'info, crate::program::FeeStake>,
    #[account(constraint = program_data.upgrade_authority_address == Some(admin.key()) @ FeeStakeError::NotUpgradeAuthority)]
    pub program_data: Account<'info, ProgramData>,
    pub system_program: Program<'info, System>,
}

pub fn init_config(ctx: Context<InitConfig>, attestor: Pubkey, fee_receiver: Pubkey, launch_fee: u64, exit_fee: u64) -> Result<()> {
    check_fees(&fee_receiver, launch_fee, exit_fee)?;
    let c = &mut ctx.accounts.config;
    c.admin = ctx.accounts.admin.key();
    c.attestor = attestor;
    c.fee_receiver = fee_receiver;
    c.launch_fee = launch_fee;
    c.exit_fee = exit_fee;
    c.bump = ctx.bumps.config;
    emit_config(c)
}

fn emit_config(c: &Config) -> Result<()> {
    emit!(ConfigChanged { admin: c.admin, attestor: c.attestor, fee_receiver: c.fee_receiver, launch_fee: c.launch_fee, exit_fee: c.exit_fee, timestamp: Clock::get()?.unix_timestamp });
    Ok(())
}

/// Admin only: where the platform's fees go and how much they are, within the
/// caps (MAX_LAUNCH_FEE, MAX_EXIT_FEE). Never touches the stakers' share.
pub fn set_fees(ctx: Context<AdminOnly>, fee_receiver: Pubkey, launch_fee: u64, exit_fee: u64) -> Result<()> {
    check_fees(&fee_receiver, launch_fee, exit_fee)?;
    let c = &mut ctx.accounts.config;
    c.fee_receiver = fee_receiver;
    c.launch_fee = launch_fee;
    c.exit_fee = exit_fee;
    emit_config(c)
}

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = admin @ FeeStakeError::NotAdmin)]
    pub config: Account<'info, Config>,
}

pub fn set_attestor(ctx: Context<AdminOnly>, attestor: Pubkey) -> Result<()> {
    let c = &mut ctx.accounts.config;
    c.attestor = attestor;
    emit_config(c)
}

pub fn set_admin(ctx: Context<AdminOnly>, admin: Pubkey) -> Result<()> {
    require!(admin != Pubkey::default(), FeeStakeError::ZeroAmount);
    let c = &mut ctx.accounts.config;
    c.admin = admin;
    emit_config(c)
}

// ------------------------------------------------------------------- stake --

/// Way in n°1, with a connected wallet: the owner signs, the coins move from
/// its token account to the vault and are credited in the same instruction.
/// Nothing is attested: the chain itself sees who sent what. The owner pays the
/// rent of its position and gets it back when the position is closed.
#[derive(Accounts)]
pub struct Stake<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, seeds = [POOL_SEED, mint.key().as_ref()], bump = pool.bump, has_one = mint)]
    pub pool: Account<'info, StakePool>,
    #[account(init_if_needed, payer = owner, space = 8 + StakePosition::INIT_SPACE, seeds = [POSITION_SEED, pool.key().as_ref(), owner.key().as_ref()], bump)]
    pub position: Account<'info, StakePosition>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = pool, associated_token::token_program = token_program, address = pool.token_vault)]
    pub token_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, token::mint = mint, token::authority = owner, token::token_program = token_program)]
    pub owner_token_account: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn stake(ctx: Context<Stake>, amount: u64) -> Result<()> {
    require!(amount > 0, FeeStakeError::ZeroAmount);
    let now = Clock::get()?.unix_timestamp;
    sync(&mut ctx.accounts.pool, now)?;
    token_interface::transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.owner_token_account.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.token_vault.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;
    let owner = ctx.accounts.owner.key();
    init_position_if_new(&mut ctx.accounts.pool, &mut ctx.accounts.position, owner, owner, ctx.bumps.position);
    let pool = &mut ctx.accounts.pool;
    let pos = &mut ctx.accounts.position;
    let mut pm = pool.to_math();
    let mut xm = pos.to_math();
    lift(pm.stake(&mut xm, amount))?;
    pool.from_math(pm);
    pos.from_math(xm);
    pool.vault_accounted = pool.vault_accounted.checked_add(amount).ok_or(FeeStakeError::MathOverflow)?;
    emit!(Staked { mint: pool.mint, owner, amount, active_from_epoch: pool.epoch_index + 1, total_pending: pool.total_pending, timestamp: now });
    Ok(())
}

// ---------------------------------------------------------- credit_deposit --

/// Way in n°2, with nothing to connect. Someone sent `amount` of the coin straight to the pool
/// address (its token account is the vault). The chain does not tell this
/// program who: the attestor read the transfer, found the sender, and attests
/// it here with the origin signature. The program bounds what it cannot
/// verify: it never credits more than the vault actually holds beyond what is
/// already attributed, and it publishes every credit with its origin so anyone
/// can check the sender against the transaction.
#[derive(Accounts)]
pub struct CreditDeposit<'info> {
    #[account(mut)]
    pub attestor: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = attestor @ FeeStakeError::NotAttestor)]
    pub config: Account<'info, Config>,
    /// CHECK: the wallet the attestor read as sender of the transfer; tokens and SOL only ever go back to it.
    pub owner: UncheckedAccount<'info>,
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, seeds = [POOL_SEED, mint.key().as_ref()], bump = pool.bump, has_one = mint)]
    pub pool: Account<'info, StakePool>,
    #[account(init_if_needed, payer = attestor, space = 8 + StakePosition::INIT_SPACE, seeds = [POSITION_SEED, pool.key().as_ref(), owner.key().as_ref()], bump)]
    pub position: Account<'info, StakePosition>,
    #[account(associated_token::mint = mint, associated_token::authority = pool, associated_token::token_program = token_program, address = pool.token_vault)]
    pub token_vault: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn credit_deposit(ctx: Context<CreditDeposit>, amount: u64, origin: [u8; 64]) -> Result<()> {
    require!(amount >= MIN_CREDIT_AMOUNT, FeeStakeError::DepositTooSmall);
    require!(ctx.accounts.owner.key() != Pubkey::default() && ctx.accounts.owner.key() != ctx.accounts.pool.key(), FeeStakeError::BadOwner);
    let now = Clock::get()?.unix_timestamp;
    sync(&mut ctx.accounts.pool, now)?;
    // Bound: everything attributed so far plus this credit must be in the vault.
    let in_vault = ctx.accounts.token_vault.amount;
    let after = ctx.accounts.pool.vault_accounted.checked_add(amount).ok_or(FeeStakeError::MathOverflow)?;
    require!(after <= in_vault, FeeStakeError::ExceedsVault);

    let owner = ctx.accounts.owner.key();
    let attestor = ctx.accounts.attestor.key();
    init_position_if_new(&mut ctx.accounts.pool, &mut ctx.accounts.position, owner, attestor, ctx.bumps.position);
    let pool = &mut ctx.accounts.pool;
    let pos = &mut ctx.accounts.position;
    let mut pm = pool.to_math();
    let mut xm = pos.to_math();
    lift(pm.stake(&mut xm, amount))?;
    pool.from_math(pm);
    pos.from_math(xm);
    pool.vault_accounted = after;
    emit!(DepositCredited { mint: pool.mint, owner, amount, origin, active_from_epoch: pool.epoch_index + 1, total_pending: pool.total_pending, timestamp: now });
    Ok(())
}

// --------------------------------------------------------- request_unstake --

/// Way out n°1, with a connected wallet: the owner signs and pays the
/// platform's exit fee; `amount` (or everything, with u64::MAX) stops earning
/// and can be withdrawn with `unstake` once the cooldown has elapsed. No crank,
/// no attestor: an owner can always leave by itself.
#[derive(Accounts)]
pub struct RequestUnstake<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    /// CHECK: the platform's wallet (`config.fee_receiver`); receives the exit fee.
    #[account(mut, address = config.fee_receiver @ FeeStakeError::BadFeeReceiver)]
    pub fee_receiver: UncheckedAccount<'info>,
    #[account(mut, seeds = [POOL_SEED, pool.mint.as_ref()], bump = pool.bump)]
    pub pool: Account<'info, StakePool>,
    #[account(mut, seeds = [POSITION_SEED, pool.key().as_ref(), owner.key().as_ref()], bump = position.bump, has_one = owner, has_one = pool)]
    pub position: Account<'info, StakePosition>,
    pub system_program: Program<'info, System>,
}

pub fn request_unstake(ctx: Context<RequestUnstake>, amount: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    sync(&mut ctx.accounts.pool, now)?;
    let fee = ctx.accounts.config.exit_fee;
    if fee > 0 {
        system_program::transfer(
            CpiContext::new(ctx.accounts.system_program.to_account_info(), system_program::Transfer { from: ctx.accounts.owner.to_account_info(), to: ctx.accounts.fee_receiver.to_account_info() }),
            fee,
        )?;
    }
    let pool = &mut ctx.accounts.pool;
    let pos = &mut ctx.accounts.position;
    let mut pm = pool.to_math();
    let mut xm = pos.to_math();
    lift(pm.activate(&mut xm))?;
    let total = PoolMath::staked_total(&xm);
    require!(total > 0, FeeStakeError::NothingStaked);
    let amount = if amount == u64::MAX { total } else { amount };
    lift(pm.request_unstake(&mut xm, amount, now, pool.cooldown_seconds))?;
    pool.from_math(pm);
    pos.from_math(xm);
    emit!(ExitRequested { mint: pool.mint, owner: pos.owner, amount, origin: [0u8; 64], fee, cooldown_until: pos.cooldown_until, total_active: pool.total_active, timestamp: now });
    Ok(())
}

// ------------------------------------------------------------ request_exit --

/// Way out n°2, with nothing to connect. The owner sent at least
/// MIN_EXIT_LAMPORTS to the pool address from the wallet that holds the
/// position; the attestor read the transfer and attests it here. Everything
/// (active + pending) goes into cooldown. The platform's exit fee is taken from
/// the lamports received in the running window (where that transfer sits when
/// the attestor is on time); if the window has already closed, the stakers were
/// paid those lamports and the platform takes nothing.
#[derive(Accounts)]
pub struct RequestExit<'info> {
    pub attestor: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = attestor @ FeeStakeError::NotAttestor)]
    pub config: Account<'info, Config>,
    /// CHECK: the platform's wallet (`config.fee_receiver`); receives the exit fee.
    #[account(mut, address = config.fee_receiver @ FeeStakeError::BadFeeReceiver)]
    pub fee_receiver: UncheckedAccount<'info>,
    /// CHECK: must equal position.owner.
    pub owner: UncheckedAccount<'info>,
    #[account(mut, seeds = [POOL_SEED, pool.mint.as_ref()], bump = pool.bump)]
    pub pool: Account<'info, StakePool>,
    #[account(mut, seeds = [POSITION_SEED, pool.key().as_ref(), owner.key().as_ref()], bump = position.bump, has_one = owner, has_one = pool)]
    pub position: Account<'info, StakePosition>,
}

pub fn request_exit(ctx: Context<RequestExit>, origin: [u8; 64]) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    sync(&mut ctx.accounts.pool, now)?;
    let total = {
        let pool = &ctx.accounts.pool;
        let mut xm = ctx.accounts.position.to_math();
        lift(pool.to_math().activate(&mut xm))?;
        ctx.accounts.position.from_math(xm);
        PoolMath::staked_total(&xm)
    };
    require!(total > 0, FeeStakeError::NothingStaked);
    let pool = &mut ctx.accounts.pool;
    let pos = &mut ctx.accounts.position;
    let mut pm = pool.to_math();
    let mut xm = pos.to_math();
    lift(pm.request_unstake(&mut xm, total, now, pool.cooldown_seconds))?;
    pool.from_math(pm);
    pos.from_math(xm);
    // The exit fee, only out of what the running window received and has not attributed yet.
    let fee = ctx.accounts.config.exit_fee.min(pool.carry);
    if fee > 0 {
        pool.carry -= fee;
        pay_from_pool(&pool.to_account_info(), &ctx.accounts.fee_receiver.to_account_info(), fee)?;
        pool.accounted_lamports = pool.accounted_lamports.saturating_sub(fee);
    }
    emit!(ExitRequested { mint: pool.mint, owner: pos.owner, amount: total, origin, fee, cooldown_until: pos.cooldown_until, total_active: pool.total_active, timestamp: now });
    Ok(())
}

// ---------------------------------------------------------------- activate --

/// Permissionless: settle a pending deposit once its window has passed.
#[derive(Accounts)]
pub struct Activate<'info> {
    #[account(mut, seeds = [POOL_SEED, pool.mint.as_ref()], bump = pool.bump)]
    pub pool: Account<'info, StakePool>,
    #[account(mut, seeds = [POSITION_SEED, pool.key().as_ref(), position.owner.as_ref()], bump = position.bump, has_one = pool)]
    pub position: Account<'info, StakePosition>,
}

pub fn activate(ctx: Context<Activate>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    sync(&mut ctx.accounts.pool, now)?;
    let pool = &ctx.accounts.pool;
    let pos = &mut ctx.accounts.position;
    let pm = pool.to_math();
    let mut xm = pos.to_math();
    let before = xm.pending;
    lift(pm.activate(&mut xm))?;
    pos.from_math(xm);
    if before > 0 && pos.pending == 0 {
        emit!(Activated { mint: pool.mint, owner: pos.owner, amount: before, timestamp: now });
    }
    Ok(())
}

// ------------------------------------------------------------------- claim --

/// Most a caller may keep from a payout it pushes: one signature's network fee.
/// A crank pushing n payouts in one transaction takes 5,000 / n from each, so
/// the payouts pay their own delivery and the stakers still get every lamport
/// of the fees minus that.
#[constant]
pub const MAX_CLAIM_FEE: u64 = 5_000;

/// Permissionless: rewards can only go to `position.owner`, so anyone (the
/// crank) may push them. `payer` pays the transaction fee and may take back at
/// most MAX_CLAIM_FEE from the payout for it.
#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: must equal position.owner (enforced below); receives the SOL.
    #[account(mut)]
    pub owner: UncheckedAccount<'info>,
    #[account(mut, seeds = [POOL_SEED, pool.mint.as_ref()], bump = pool.bump)]
    pub pool: Account<'info, StakePool>,
    #[account(mut, seeds = [POSITION_SEED, pool.key().as_ref(), owner.key().as_ref()], bump = position.bump, has_one = owner, has_one = pool)]
    pub position: Account<'info, StakePosition>,
}

pub fn claim(ctx: Context<Claim>, fee: u64) -> Result<()> {
    require!(fee <= MAX_CLAIM_FEE, FeeStakeError::ClaimFeeTooHigh);
    let now = Clock::get()?.unix_timestamp;
    sync(&mut ctx.accounts.pool, now)?;
    let pool = &mut ctx.accounts.pool;
    let pos = &mut ctx.accounts.position;
    let mut pm = pool.to_math();
    let mut xm = pos.to_math();
    let owed = lift(pm.claim(&mut xm))?;
    // the owner pushing its own payout keeps everything
    let fee = if ctx.accounts.payer.key() == pos.owner { 0 } else { fee };
    require!(owed > fee, FeeStakeError::NothingToClaim);
    pool.from_math(pm);
    pos.from_math(xm);
    let net = owed - fee;
    pos.total_claimed = pos.total_claimed.saturating_add(net);
    pay_from_pool(&pool.to_account_info(), &ctx.accounts.owner.to_account_info(), net)?;
    pay_from_pool(&pool.to_account_info(), &ctx.accounts.payer.to_account_info(), fee)?;
    pool.accounted_lamports = pool.accounted_lamports.saturating_sub(owed);
    emit!(Claimed { mint: pool.mint, owner: pos.owner, lamports: net, fee, timestamp: now });
    Ok(())
}

// ---------------------------------------------------------- close_position --

/// Permissionless: an empty position (nothing active, pending or leaving) is
/// closed. Whatever it is still owed is paid to its owner first, in full; the
/// account's rent goes back to whoever paid it: the owner (wallet stake) or the
/// attestor (first deposit credited from a transfer). A wallet that stakes
/// again later simply gets a new one.
#[derive(Accounts)]
pub struct ClosePosition<'info> {
    /// CHECK: receives the rent; must be the account that paid it.
    #[account(mut, address = position.rent_payer @ FeeStakeError::NotRentPayer)]
    pub rent_receiver: UncheckedAccount<'info>,
    /// CHECK: must equal position.owner; receives what is still owed.
    #[account(mut)]
    pub owner: UncheckedAccount<'info>,
    #[account(mut, seeds = [POOL_SEED, pool.mint.as_ref()], bump = pool.bump)]
    pub pool: Account<'info, StakePool>,
    #[account(mut, close = rent_receiver, seeds = [POSITION_SEED, pool.key().as_ref(), owner.key().as_ref()], bump = position.bump, has_one = owner, has_one = pool)]
    pub position: Account<'info, StakePosition>,
}

pub fn close_position(ctx: Context<ClosePosition>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    sync(&mut ctx.accounts.pool, now)?;
    let pool = &mut ctx.accounts.pool;
    let pos = &mut ctx.accounts.position;
    require!(pos.amount == 0 && pos.pending == 0 && pos.cooldown_amount == 0, FeeStakeError::PositionNotEmpty);
    let mut pm = pool.to_math();
    let mut xm = pos.to_math();
    let owed = lift(pm.claim(&mut xm))?;
    pool.from_math(pm);
    if owed > 0 {
        pay_from_pool(&pool.to_account_info(), &ctx.accounts.owner.to_account_info(), owed)?;
        pool.accounted_lamports = pool.accounted_lamports.saturating_sub(owed);
    }
    let total_claimed = pos.total_claimed.saturating_add(owed);
    pool.staker_count = pool.staker_count.saturating_sub(1);
    emit!(PositionClosed { mint: pool.mint, owner: pos.owner, total_claimed, last_payout: owed, timestamp: now });
    Ok(())
}

// ----------------------------------------------------------------- unstake --

/// Permissionless once the cooldown has elapsed: tokens can only go to
/// `owner`'s token account (created by `payer` if needed).
#[derive(Accounts)]
pub struct Unstake<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: must equal position.owner; receives the tokens.
    pub owner: UncheckedAccount<'info>,
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, seeds = [POOL_SEED, mint.key().as_ref()], bump = pool.bump, has_one = mint)]
    pub pool: Account<'info, StakePool>,
    #[account(mut, seeds = [POSITION_SEED, pool.key().as_ref(), owner.key().as_ref()], bump = position.bump, has_one = owner)]
    pub position: Account<'info, StakePosition>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = pool, associated_token::token_program = token_program, address = pool.token_vault)]
    pub token_vault: InterfaceAccount<'info, TokenAccount>,
    #[account(init_if_needed, payer = payer, associated_token::mint = mint, associated_token::authority = owner, associated_token::token_program = token_program)]
    pub owner_token_account: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn unstake(ctx: Context<Unstake>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    sync(&mut ctx.accounts.pool, now)?;
    let mint_info = ctx.accounts.mint.to_account_info();
    let decimals = ctx.accounts.mint.decimals;
    let token_program = ctx.accounts.token_program.to_account_info();
    let vault = ctx.accounts.token_vault.to_account_info();
    let owner_ata = ctx.accounts.owner_token_account.to_account_info();
    let pool = &mut ctx.accounts.pool;
    let pos = &mut ctx.accounts.position;
    let mut pm = pool.to_math();
    let mut xm = pos.to_math();
    let out = lift(pm.unstake(&mut xm, now))?;
    pool.from_math(pm);
    pos.from_math(xm);
    pool.vault_accounted = pool.vault_accounted.saturating_sub(out);

    let mint_key = pool.mint;
    let bump = pool.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[POOL_SEED, mint_key.as_ref(), &[bump]]];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            token_program,
            TransferChecked { from: vault, mint: mint_info, to: owner_ata, authority: pool.to_account_info() },
            signer_seeds,
        ),
        out,
        decimals,
    )?;
    emit!(Unstaked { mint: mint_key, owner: pos.owner, amount: out, timestamp: now });
    Ok(())
}

// ---------------------------------------------------------- lock_fee_split --

/// For coins created on pump.fun with `creator = ["creator", mint]` PDA: run
/// the fee program's `create_fee_sharing_config` (optional, when the config
/// does not exist yet) and `update_fee_shares_v2` with that PDA signing as the
/// coin's creator / sharing-config admin. The caller supplies the two instructions'
/// account lists (remaining accounts: first `create_accounts` for the create
/// call, the rest for the update) and their data; this program only lets them
/// through if the program id, discriminators and shares are exactly what the
/// stakers were promised: 100 % to the pool, nobody else, ever.
///
/// Since the creator PDA is the only key that can sign for these instructions
/// and this program exposes no other way to use it, the split is final.
#[derive(Accounts)]
pub struct LockFeeSplit<'info> {
    /// Pays the rent the fee program needs (sent to the creator PDA first, which pays as signer).
    #[account(mut)]
    pub payer: Signer<'info>,
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(seeds = [POOL_SEED, mint.key().as_ref()], bump = pool.bump, has_one = mint)]
    pub pool: Account<'info, StakePool>,
    /// CHECK: system-owned PDA, the coin's creator on pump.fun; signs the CPIs below.
    #[account(mut, seeds = [CREATOR_SEED, mint.key().as_ref()], bump)]
    pub creator_authority: UncheckedAccount<'info>,
    /// CHECK: the pump.fun fee program, pinned by address.
    #[account(address = PUMP_FEE_PROGRAM_ID)]
    pub fee_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// Pure validation of the `update_fee_shares_v2` payload the caller supplies:
/// discriminator, then exactly one share: `{pool: 10 000 bps}`, and no
/// trailing bytes. Host-tested with random inputs below.
pub fn check_update_data(update_data: &[u8], pool_key: &Pubkey) -> Result<()> {
    require!(update_data.len() >= 8 + 4 && update_data[..8] == UPDATE_FEE_SHARES_V2_DISC, FeeStakeError::BadCpi);
    require!(update_data.len() == 12 + 34, FeeStakeError::BadSplit);
    let n = u32::from_le_bytes(update_data[8..12].try_into().unwrap());
    require!(n == 1, FeeStakeError::BadSplit);
    let addr = Pubkey::try_from(&update_data[12..44]).map_err(|_| FeeStakeError::BadSplit)?;
    let bps = u16::from_le_bytes(update_data[44..46].try_into().unwrap());
    require!(addr == *pool_key && bps == 10_000, FeeStakeError::BadSplit);
    Ok(())
}

/// The `create_fee_sharing_config` payload is its discriminator and nothing else.
pub fn check_create_data(create_data: &[u8]) -> Result<()> {
    require!(create_data == CREATE_FEE_SHARING_CONFIG_DISC, FeeStakeError::BadCpi);
    Ok(())
}

pub fn lock_fee_split<'info>(ctx: Context<'_, '_, 'info, 'info, LockFeeSplit<'info>>, create_accounts: u8, create_data: Vec<u8>, update_data: Vec<u8>, fund_lamports: u64) -> Result<()> {
    let pool_key = ctx.accounts.pool.key();
    let mint_key = ctx.accounts.mint.key();
    let creator_key = ctx.accounts.creator_authority.key();
    let bump = ctx.bumps.creator_authority;
    let remaining = ctx.remaining_accounts;
    let n_create = create_accounts as usize;
    require!(remaining.len() >= n_create, FeeStakeError::BadCpi);
    let (create_infos, update_infos) = remaining.split_at(n_create);

    // 1. shares: 100 % to the pool, nothing else.
    check_update_data(&update_data, &pool_key)?;
    if n_create > 0 {
        check_create_data(&create_data)?;
    }

    // 2. the creator PDA pays the fee program's rent: top it up from payer first.
    if fund_lamports > 0 {
        system_program::transfer(
            CpiContext::new(ctx.accounts.system_program.to_account_info(), system_program::Transfer { from: ctx.accounts.payer.to_account_info(), to: ctx.accounts.creator_authority.to_account_info() }),
            fund_lamports,
        )?;
    }

    // 3. account metas: the creator PDA is the only signer we lend; every other signer flag is refused.
    let metas = |infos: &[AccountInfo<'info>]| -> Result<Vec<AccountMeta>> {
        let mut v = Vec::with_capacity(infos.len());
        for a in infos {
            require!(a.key() != crate::ID, FeeStakeError::BadCpi); // never re-enter ourselves
            let is_signer = a.key() == creator_key;
            require!(!a.is_signer || is_signer, FeeStakeError::BadCpi);
            v.push(if a.is_writable { AccountMeta::new(a.key(), is_signer) } else { AccountMeta::new_readonly(a.key(), is_signer) });
        }
        Ok(v)
    };
    let seeds: &[&[&[u8]]] = &[&[CREATOR_SEED, mint_key.as_ref(), &[bump]]];
    let mut all_infos: Vec<AccountInfo<'info>> = Vec::with_capacity(remaining.len() + 2);
    all_infos.push(ctx.accounts.fee_program.to_account_info());
    all_infos.push(ctx.accounts.creator_authority.to_account_info());

    if n_create > 0 {
        let ix = anchor_lang::solana_program::instruction::Instruction { program_id: PUMP_FEE_PROGRAM_ID, accounts: metas(create_infos)?, data: create_data };
        let mut infos = all_infos.clone();
        infos.extend_from_slice(create_infos);
        anchor_lang::solana_program::program::invoke_signed(&ix, &infos, seeds)?;
    }
    let ix = anchor_lang::solana_program::instruction::Instruction { program_id: PUMP_FEE_PROGRAM_ID, accounts: metas(update_infos)?, data: update_data };
    all_infos.extend_from_slice(update_infos);
    anchor_lang::solana_program::program::invoke_signed(&ix, &all_infos, seeds)?;

    emit!(FeeSplitLocked { mint: mint_key, pool: pool_key, pool_bps: 10_000, timestamp: Clock::get()?.unix_timestamp });
    Ok(())
}

#[cfg(test)]
mod split_fuzz {
    //! Random and mutated payloads against `check_update_data` / `check_create_data`:
    //! only the canonical encoding of {pool: 100 %} may pass.
    use super::*;
    use proptest::prelude::*;

    fn encode(shares: &[(Pubkey, u16)]) -> Vec<u8> {
        let mut v = Vec::with_capacity(12 + shares.len() * 34);
        v.extend_from_slice(&UPDATE_FEE_SHARES_V2_DISC);
        v.extend_from_slice(&(shares.len() as u32).to_le_bytes());
        for (k, bps) in shares {
            v.extend_from_slice(k.as_ref());
            v.extend_from_slice(&bps.to_le_bytes());
        }
        v
    }
    fn pool() -> Pubkey { Pubkey::new_from_array([7u8; 32]) }
    fn full() -> Vec<u8> { encode(&[(pool(), 10_000)]) }

    #[test]
    fn canonical_passes() {
        assert!(check_update_data(&full(), &pool()).is_ok(), "100 % to the pool");
        assert!(check_create_data(&CREATE_FEE_SHARING_CONFIG_DISC).is_ok());
    }

    #[test]
    fn other_splits_fail() {
        let p = pool();
        let other = Pubkey::new_from_array([9u8; 32]);
        assert!(check_update_data(&encode(&[(other, 10_000)]), &p).is_err(), "100 % to someone else");
        assert!(check_update_data(&encode(&[(p, 9_999)]), &p).is_err(), "less than 100 %");
        assert!(check_update_data(&encode(&[(p, 8_000), (other, 2_000)]), &p).is_err(), "a launcher share");
        assert!(check_update_data(&encode(&[(p, 10_000), (other, 0)]), &p).is_err(), "a second share, even at 0 %");
        assert!(check_update_data(&encode(&[]), &p).is_err(), "no share");
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

        /// Random bytes of any length never pass unless they ARE the canonical encoding.
        #[test]
        fn random_update_payloads_are_refused(bytes in proptest::collection::vec(any::<u8>(), 0..120)) {
            if check_update_data(&bytes, &pool()).is_ok() { prop_assert!(bytes == full()); }
        }

        /// Flip any single byte of the canonical payload: refused (every byte is load-bearing).
        #[test]
        fn any_single_byte_mutation_is_refused(idx in 0usize..(12 + 34), xor in 1u8..=255) {
            let mut f = full();
            f[idx] ^= xor;
            prop_assert!(check_update_data(&f, &pool()).is_err(), "mutation at {idx} passed");
        }

        /// Append or drop bytes: refused.
        #[test]
        fn length_changes_are_refused(extra in proptest::collection::vec(any::<u8>(), 1..8), cut in 1usize..(12 + 34)) {
            let base = full();
            let mut longer = base.clone();
            longer.extend_from_slice(&extra);
            prop_assert!(check_update_data(&longer, &pool()).is_err());
            prop_assert!(check_update_data(&base[..base.len() - cut], &pool()).is_err());
        }

        /// Any bps other than 10 000 is refused.
        #[test]
        fn other_bps_are_refused(bps in any::<u16>()) {
            prop_assume!(bps != 10_000);
            prop_assert!(check_update_data(&encode(&[(pool(), bps)]), &pool()).is_err());
        }

        #[test]
        fn random_create_payloads_are_refused(bytes in proptest::collection::vec(any::<u8>(), 0..24)) {
            if check_create_data(&bytes).is_ok() { prop_assert!(bytes == CREATE_FEE_SHARING_CONFIG_DISC); }
        }
    }
}

// ------------------------------------------------------------- rescue_token --

/// Admin only: send back a token that someone sent to a pool address by
/// mistake (a different coin than the pool's). The pool's own coin can never
/// leave this way (it belongs to the stakers), and SOL is never touched.
#[derive(Accounts)]
pub struct RescueToken<'info> {
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin @ FeeStakeError::NotAdmin)]
    pub config: Account<'info, Config>,
    #[account(seeds = [POOL_SEED, pool.mint.as_ref()], bump = pool.bump)]
    pub pool: Account<'info, StakePool>,
    #[account(constraint = stray_mint.key() != pool.mint @ FeeStakeError::NotAStrayToken)]
    pub stray_mint: InterfaceAccount<'info, Mint>,
    #[account(mut, token::mint = stray_mint, token::authority = pool, token::token_program = token_program,
              constraint = from.key() != pool.token_vault @ FeeStakeError::NotAStrayToken)]
    pub from: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, token::mint = stray_mint, token::token_program = token_program)]
    pub to: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn rescue_token(ctx: Context<RescueToken>) -> Result<()> {
    let amount = ctx.accounts.from.amount;
    require!(amount > 0, FeeStakeError::ZeroAmount);
    let mint_key = ctx.accounts.pool.mint;
    let bump = ctx.accounts.pool.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[POOL_SEED, mint_key.as_ref(), &[bump]]];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.from.to_account_info(),
                mint: ctx.accounts.stray_mint.to_account_info(),
                to: ctx.accounts.to.to_account_info(),
                authority: ctx.accounts.pool.to_account_info(),
            },
            signer_seeds,
        ),
        amount,
        ctx.accounts.stray_mint.decimals,
    )?;
    emit!(StrayTokenReturned {
        pool: ctx.accounts.pool.key(),
        mint: ctx.accounts.stray_mint.key(),
        to: ctx.accounts.to.key(),
        amount,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
