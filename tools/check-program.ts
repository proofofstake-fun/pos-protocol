/**
 * Who can still change the Proof of Stake program, and what the platform charges.
 *
 *   RPC_URL=https://api.mainnet-beta.solana.com npx tsx tools/check-program.ts
 */
import { PublicKey } from "@solana/web3.js";
import { FEE_STAKE_PROGRAM_ID, findConfigPda, findProgramDataPda } from "../sdk/pda";
import { connection, program, sol } from "./common";

async function main() {
  console.log(`\nProof of Stake program ${FEE_STAKE_PROGRAM_ID.toBase58()}\n`);
  const prog = await connection.getAccountInfo(FEE_STAKE_PROGRAM_ID);
  if (!prog?.executable) throw new Error("not deployed on this cluster");
  const data = await connection.getAccountInfo(findProgramDataPda(FEE_STAKE_PROGRAM_ID));
  // ProgramData: u32 tag (3), u64 slot, Option<Pubkey> authority
  const hasAuthority = data!.data[12] === 1;
  const authority = hasAuthority ? new PublicKey(data!.data.subarray(13, 45)).toBase58() : "none (the program can never change again)";
  console.log(`  upgrade authority   ${authority}`);
  console.log(`  last deployed slot  ${data!.data.readBigUInt64LE(4)}`);

  const c: any = await (program.account as any).config.fetchNullable(findConfigPda()[0]);
  if (!c) { console.log("  config              not initialised"); return; }
  console.log(`  admin               ${c.admin.toBase58()}`);
  console.log(`  attestor            ${c.attestor.toBase58()}   (only for stakes made by sending coins to a pool address)`);
  console.log(`  fee receiver        ${c.feeReceiver.toBase58()}`);
  console.log(`  launch fee          ${sol(c.launchFee)}   (capped at 0.2 SOL by the program)`);
  console.log(`  exit fee            ${sol(c.exitFee)}   (capped at 0.05 SOL by the program)`);
  console.log(`  share of trading fees taken by the platform: 0%, the program accepts no shareholder other than the pool\n`);
}

main().catch((e) => { console.error(`\n  ✘ ${e?.message ?? e}\n`); process.exit(1); });
