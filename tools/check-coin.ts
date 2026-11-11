/**
 * Check, from the chain alone, that a coin's trading fees go 100% to its stakers.
 *
 *   RPC_URL=https://api.mainnet-beta.solana.com npx tsx tools/check-coin.ts <coin mint>
 */
import { PublicKey } from "@solana/web3.js";
import { findCreatorAuthority, findStakePoolPda, findStakeVault, tokenProgramOf } from "../sdk/pda";
import { connection, ok, program, sol } from "./common";

const PUMP = new PublicKey("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
const PUMP_FEES = new PublicKey("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");

/** pump.fun's SharingConfig: 8 discriminator, u8 bump, u8 version, u8 status, Pubkey mint, Pubkey admin, bool admin_revoked, Vec<{Pubkey, u16}> shareholders. */
function decodeSharingConfig(d: Buffer) {
  let o = 8 + 3;
  const mint = new PublicKey(d.subarray(o, o + 32)); o += 32;
  const admin = new PublicKey(d.subarray(o, o + 32)); o += 32;
  const adminRevoked = d[o] === 1; o += 1;
  const n = d.readUInt32LE(o); o += 4;
  const shareholders = Array.from({ length: n }, () => {
    const address = new PublicKey(d.subarray(o, o + 32)); o += 32;
    const bps = d.readUInt16LE(o); o += 2;
    return { address, bps };
  });
  return { mint, admin, adminRevoked, shareholders };
}

async function main() {
  const arg = process.argv[2];
  if (!arg) throw new Error("usage: npx tsx tools/check-coin.ts <coin mint>");
  const mint = new PublicKey(arg);
  const [pool] = findStakePoolPda(mint);
  const [creator] = findCreatorAuthority(mint);
  const [curve] = PublicKey.findProgramAddressSync([Buffer.from("bonding-curve"), mint.toBuffer()], PUMP);
  const [sharing] = PublicKey.findProgramAddressSync([Buffer.from("sharing-config"), mint.toBuffer()], PUMP_FEES);
  console.log(`\ncoin            ${mint.toBase58()}`);
  console.log(`staking pool    ${pool.toBase58()}   (no private key: owned by the program)`);
  console.log(`creator address ${creator.toBase58()}   (no private key: owned by the program)`);
  console.log(`fee sharing     ${sharing.toBase58()}   (pump.fun)\n`);

  const [curveAcc, sharingAcc] = await connection.getMultipleAccountsInfo([curve, sharing]);
  if (curveAcc) {
    // BondingCurve: 8 discriminator, 5 × u64, bool complete, Pubkey creator
    const curveCreator = new PublicKey(curveAcc.data.subarray(8 + 5 * 8 + 1, 8 + 5 * 8 + 1 + 32));
    const complete = curveAcc.data[8 + 5 * 8] === 1;
    ok(curveCreator.equals(creator) || curveCreator.equals(sharing), "pump.fun creator is the program's address (or its locked fee-sharing account)", curveCreator.toBase58());
    console.log(`  · bonding curve ${complete ? "complete: the coin trades on PumpSwap, fees still flow to the pool" : "active"}`);
  } else {
    ok(false, "pump.fun bonding curve found");
  }

  if (!sharingAcc) {
    ok(false, "pump.fun fee-sharing account exists");
  } else {
    const s = decodeSharingConfig(sharingAcc.data);
    const only = s.shareholders.length === 1 && s.shareholders[0].address.equals(pool) && s.shareholders[0].bps === 10_000;
    ok(only, "fee split is exactly { staking pool: 100% }", s.shareholders.map((x) => `${x.address.toBase58().slice(0, 6)}… ${x.bps / 100}%`).join(", "));
    ok(s.adminRevoked, "admin revoked: nobody can ever change the split", s.adminRevoked ? "" : `admin ${s.admin.toBase58()}`);
  }

  const p: any = await (program.account as any).stakePool.fetchNullable(pool);
  if (!p) { ok(false, "staking pool open"); return; }
  const tp = await tokenProgramOf(connection, mint);
  const vault = await connection.getTokenAccountBalance(findStakeVault(mint, pool, tp)).catch(() => null);
  ok(true, "staking pool open");
  console.log(`  · stakers ${p.stakerCount} · staked ${vault ? vault.value.uiAmountString : "?"} coins · paid to stakers so far ${sol(p.totalDistributed)}`);
  console.log(`  · windows ${p.epochSeconds} s · leaving delay ${Math.round(Number(p.cooldownSeconds) / 60)} min\n`);
}

main().catch((e) => { console.error(`\n  ✘ ${e?.message ?? e}\n`); process.exit(1); });
