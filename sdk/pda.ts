/** Addresses of the Proof of Stake program (all derived, none holds a private key). */
import { PublicKey, type Connection } from "@solana/web3.js";
import { getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID } from "@solana/spl-token";

export const FEE_STAKE_PROGRAM_ID = new PublicKey("PoSBsxAZVhLmPAUv9YLcWHauVjzSro6smzt92JPWtLY");

export const POOL_SEED = Buffer.from("pool");
export const POSITION_SEED = Buffer.from("position");
export const CREATOR_SEED = Buffer.from("creator");
export const CONFIG_SEED = Buffer.from("config");

/** A coin's staking pool: where its creator fees land, and the address people send the coin to. */
export const findStakePoolPda = (mint: PublicKey, programId = FEE_STAKE_PROGRAM_ID): [PublicKey, number] =>
  PublicKey.findProgramAddressSync([POOL_SEED, mint.toBuffer()], programId);

/** One wallet's position in one pool. */
export const findStakePositionPda = (pool: PublicKey, owner: PublicKey, programId = FEE_STAKE_PROGRAM_ID): [PublicKey, number] =>
  PublicKey.findProgramAddressSync([POSITION_SEED, pool.toBuffer(), owner.toBuffer()], programId);

/** The pool's token account (holds the staked coins). */
export const findStakeVault = (mint: PublicKey, pool: PublicKey, tokenProgram: PublicKey = TOKEN_PROGRAM_ID): PublicKey =>
  getAssociatedTokenAddressSync(mint, pool, true, tokenProgram);

/** The coin's pump.fun creator: signs the fee split once, in `lock_fee_split`, and nothing else. */
export const findCreatorAuthority = (mint: PublicKey, programId = FEE_STAKE_PROGRAM_ID): [PublicKey, number] =>
  PublicKey.findProgramAddressSync([CREATOR_SEED, mint.toBuffer()], programId);

/** The program's config: admin, attestor, fee receiver, the two flat fees. */
export const findConfigPda = (programId = FEE_STAKE_PROGRAM_ID): [PublicKey, number] =>
  PublicKey.findProgramAddressSync([CONFIG_SEED], programId);

export const findUserAta = (mint: PublicKey, user: PublicKey, tokenProgram: PublicKey = TOKEN_PROGRAM_ID): PublicKey =>
  getAssociatedTokenAddressSync(mint, user, true, tokenProgram);

/** The upgradeable loader's data account of a program (holds its upgrade authority). */
export const BPF_LOADER_UPGRADEABLE = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");
export function findProgramDataPda(programId: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync([programId.toBuffer()], BPF_LOADER_UPGRADEABLE)[0];
}

/** The token program that owns a mint (pump.fun coins are Token-2022). */
export async function tokenProgramOf(connection: Connection, mint: PublicKey): Promise<PublicKey> {
  const info = await connection.getAccountInfo(mint);
  if (!info) throw new Error(`mint ${mint.toBase58()} not found`);
  return info.owner;
}
