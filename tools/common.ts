import { AnchorProvider, Program, Wallet, type Idl } from "@coral-xyz/anchor";
import { Connection, Keypair } from "@solana/web3.js";
import idl from "../sdk/idl/fee_stake.json";
import { FEE_STAKE_PROGRAM_ID } from "../sdk/pda";

export const RPC_URL = process.env.RPC_URL ?? "https://api.mainnet-beta.solana.com";
export const connection = new Connection(RPC_URL, "confirmed");
/** Read-only: a throwaway key, nothing is ever signed by these tools. */
export const program = new Program({ ...(idl as Idl), address: FEE_STAKE_PROGRAM_ID.toBase58() } as Idl, new AnchorProvider(connection, new Wallet(Keypair.generate()), {}));
export const sol = (lamports: bigint | number | { toString(): string }) => `${(Number(lamports.toString()) / 1e9).toLocaleString("en-US", { maximumFractionDigits: 6 })} SOL`;
export const ok = (good: boolean, what: string, detail = "") => console.log(`  ${good ? "✔" : "✘"} ${what}${detail ? `  ${detail}` : ""}`);
