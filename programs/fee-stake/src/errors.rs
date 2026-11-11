use anchor_lang::prelude::*;
use stake_math::StakeError;

#[error_code]
pub enum FeeStakeError {
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Not enough staked tokens")]
    InsufficientStake,
    #[msg("Cooldown has not elapsed yet")]
    CooldownActive,
    #[msg("Nothing to unstake")]
    NothingToUnstake,
    #[msg("Nothing to claim")]
    NothingToClaim,
    #[msg("Math overflow")]
    MathOverflow,
    #[msg("Pool would drop below rent exemption")]
    RentViolation,
    #[msg("Invalid cooldown")]
    InvalidCooldown,
    #[msg("Invalid epoch length")]
    InvalidEpoch,
    #[msg("Nothing staked")]
    NothingStaked,
    #[msg("Deposit below the minimum of 1,000 tokens")]
    DepositTooSmall,
    #[msg("Not a pump.fun bonding curve for this mint")]
    NotACurve,
    #[msg("Only the coin's creator (pump.fun bonding curve creator) can open its pool")]
    NotTheCreator,
    #[msg("Fee-program CPI refused: wrong program, discriminator or signer")]
    BadCpi,
    #[msg("Fee split must be exactly 100 % to the pool")]
    BadSplit,
    #[msg("Mint carries a Token-2022 extension the pool cannot work with (transfer fee, hook, permanent delegate, ...)")]
    UnsafeMint,
    #[msg("Only the attestor (config.attestor) may attribute deposits and exits")]
    NotAttestor,
    #[msg("Only the admin (config.admin) may change the config")]
    NotAdmin,
    #[msg("Credit would exceed the tokens actually held by the vault")]
    ExceedsVault,
    #[msg("Owner must be a real wallet address")]
    BadOwner,
    #[msg("Platform fee above the cap (launch 0.2 SOL, exit 0.05 SOL)")]
    FeeTooHigh,
    #[msg("Fee receiver must be the wallet set in the config")]
    BadFeeReceiver,
    #[msg("The rent goes back to the account that paid it")]
    NotRentPayer,
    #[msg("A caller may keep at most 5,000 lamports from a payout")]
    ClaimFeeTooHigh,
    #[msg("Only an empty position (nothing active, pending or leaving) can be closed")]
    PositionNotEmpty,
    #[msg("Only the program's upgrade authority can create the config")]
    NotUpgradeAuthority,
    #[msg("Only a token other than the pool's own coin can be sent back this way")]
    NotAStrayToken,
}

impl From<StakeError> for FeeStakeError {
    fn from(e: StakeError) -> Self {
        match e {
            StakeError::Overflow => FeeStakeError::MathOverflow,
            StakeError::ZeroAmount => FeeStakeError::ZeroAmount,
            StakeError::InsufficientStake => FeeStakeError::InsufficientStake,
            StakeError::CooldownActive => FeeStakeError::CooldownActive,
            StakeError::NothingToUnstake => FeeStakeError::NothingToUnstake,
            StakeError::InvalidParams => FeeStakeError::InvalidEpoch,
        }
    }
}

/// Shorthand to lift a `stake_math` result into an Anchor result.
pub fn lift<T>(r: core::result::Result<T, StakeError>) -> Result<T> {
    r.map_err(|e| error!(FeeStakeError::from(e)))
}
