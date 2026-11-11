/**
 * End-to-end test of the staking program on a local validator: `anchor test`.
 * Uses 60-second windows (the program's minimum) so boundaries can be waited
 * for. pump.fun is not needed: its distribute pays shareholders with a plain
 * SOL transfer, simulated here with SystemProgram.transfer to the pool.
 * Runs ~4 minutes.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { createAssociatedTokenAccount, createInitializeMintInstruction, createInitializeTransferFeeConfigInstruction, createMint, ExtensionType, getAccount, getMintLen, getOrCreateAssociatedTokenAccount, mintTo, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, transfer } from "@solana/spl-token";
import { assert } from "chai";
import { PublicKey } from "@solana/web3.js";
import { FeeStake } from "../target/types/fee_stake";
import { findConfigPda, findCreatorAuthority, findStakePoolPda, findStakePositionPda, findStakeVault, findUserAta, findProgramDataPda } from "../sdk/pda";

const { Keypair, LAMPORTS_PER_SOL, SystemProgram, Transaction } = anchor.web3;
const T = 1_000_000n;
const EPOCH = 60;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const now = () => Math.floor(Date.now() / 1000);
/** Sleep until `n` boundaries of EPOCH have passed (plus a 2 s margin). */
const nextBoundary = async (n = 1) => {
  const target = (Math.floor(now() / EPOCH) + n) * EPOCH + 2;
  await sleep(Math.max(0, target - now()) * 1000);
};

describe("fee_stake (1-min windows; stake with a wallet or by sending to the pool address)", function () {
  this.timeout(12 * 60 * 1000);
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.FeeStake as Program<FeeStake>;
  const payer = (provider.wallet as anchor.Wallet).payer;

  const alice = Keypair.generate(); // stakes by sending tokens to the pool address (the attestor credits her)
  const bob = Keypair.generate(); // stakes with his wallet (signs `stake`)
  const carol = Keypair.generate(); // stakes with her wallet and stays to the end
  const attestor = Keypair.generate(); // the crank's attribution key
  const stranger = Keypair.generate();
  const platform = Keypair.generate(); // config.fee_receiver: launch fee + exit fees
  const LAUNCH_FEE = 50_000_000n; // 0.05 SOL
  const EXIT_FEE = 10_000_000n; // 0.01 SOL
  const ALL = new anchor.BN("18446744073709551615"); // u64::MAX = everything
  let mint: anchor.web3.PublicKey;
  let pool: anchor.web3.PublicKey;
  let vault: anchor.web3.PublicKey;
  const [config] = findConfigPda(program.programId);
  const sigBytes = (sig: string) => Array.from(anchor.utils.bytes.bs58.decode(sig));
  const bn = (v: bigint | number) => new anchor.BN(v.toString());

  const airdrop = async (pk: anchor.web3.PublicKey, sol: number) => {
    const sig = await provider.connection.requestAirdrop(pk, sol * LAMPORTS_PER_SOL);
    await provider.connection.confirmTransaction(sig, "confirmed");
  };
  const balance = async (pk: anchor.web3.PublicKey) => BigInt(await provider.connection.getBalance(pk));
  const simulateFees = async (sol: number) => {
    const tx = new Transaction().add(SystemProgram.transfer({ fromPubkey: payer.publicKey, toPubkey: pool, lamports: sol * LAMPORTS_PER_SOL }));
    await provider.sendAndConfirm(tx, [payer]);
  };
  const harvest = () => program.methods.harvest().accounts({ pool }).rpc();
  const position = (owner: anchor.web3.PublicKey) => findStakePositionPda(pool, owner, program.programId)[0];
  const pos = (owner: anchor.web3.PublicKey): Promise<any> => program.account.stakePosition.fetch(position(owner));
  const poolAcc = (): Promise<any> => program.account.stakePool.fetch(pool);
  const refuse = async (what: string, f: () => Promise<unknown>, re: RegExp) => {
    try { await f(); } catch (e: any) { assert.match(e.toString(), re, what); return; }
    assert.fail(`${what}: accepted`);
  };
  const anyErr = /Error|error|failed|Constraint|AccountNotInitialized|seeds|has one|0x/;
  const claimFor = (owner: anchor.web3.PublicKey, fee = 0, by = payer) =>
    program.methods.claim(new anchor.BN(fee)).accounts({ payer: by.publicKey, owner, pool, position: position(owner) }).signers(by === payer ? [] : [by]).rpc();
  const closeFor = (owner: anchor.web3.PublicKey, rentReceiver: anchor.web3.PublicKey) =>
    program.methods.closePosition().accounts({ rentReceiver, owner, pool, position: position(owner) } as any).rpc();
  const initPool = (m: anchor.web3.PublicKey, bondingCurve: anchor.web3.PublicKey, epoch: number | null, cooldown: number | null, tokenProgram = TOKEN_PROGRAM_ID) =>
    program.methods.initPool(epoch === null ? null : new anchor.BN(epoch), cooldown === null ? null : new anchor.BN(cooldown)).accounts({ creator: payer.publicKey, mint: m, bondingCurve, feeReceiver: platform.publicKey, tokenProgram } as any).rpc();

  /** Transfer path: a plain SPL transfer to the vault, nothing else. */
  const sendToPool = async (from: anchor.web3.Keypair, amount: bigint) => transfer(provider.connection, from, findUserAta(mint, from.publicKey), vault, from, amount);
  /** The crank: credit `owner` for the transfer `sig`. */
  const credit = (owner: anchor.web3.PublicKey, amount: bigint, sig: string, signer = attestor) =>
    program.methods.creditDeposit(bn(amount), sigBytes(sig)).accounts({ attestor: signer.publicKey, owner, mint, pool, tokenVault: vault, tokenProgram: TOKEN_PROGRAM_ID }).signers([signer]).rpc();
  const requestExit = (owner: anchor.web3.PublicKey, sig: string, signer = attestor) =>
    program.methods.requestExit(sigBytes(sig)).accounts({ attestor: signer.publicKey, feeReceiver: platform.publicKey, owner, pool, position: position(owner) } as any).signers([signer]).rpc();
  /** Wallet path: the owner signs. */
  const stake = (who: anchor.web3.Keypair, amount: bigint, from = findUserAta(mint, who.publicKey)) =>
    program.methods.stake(bn(amount)).accounts({ owner: who.publicKey, mint, pool, tokenVault: vault, ownerTokenAccount: from, tokenProgram: TOKEN_PROGRAM_ID } as any).signers([who]).rpc();
  const requestUnstake = (who: anchor.web3.Keypair, amount: anchor.BN, owner = who.publicKey) =>
    program.methods.requestUnstake(amount).accounts({ owner, feeReceiver: platform.publicKey, pool, position: position(owner) } as any).signers([who]).rpc();
  const unstakeFor = (owner: anchor.web3.PublicKey, by = payer) =>
    program.methods.unstake().accounts({ payer: by.publicKey, owner, mint, pool, tokenProgram: TOKEN_PROGRAM_ID }).signers(by === payer ? [] : [by]).rpc();

  before(async () => {
    await Promise.all([airdrop(alice.publicKey, 5), airdrop(bob.publicKey, 5), airdrop(carol.publicKey, 5), airdrop(attestor.publicKey, 2), airdrop(stranger.publicKey, 2), airdrop(platform.publicKey, 1)]);
    mint = await createMint(provider.connection, payer, payer.publicKey, null, 6);
    for (const u of [alice, bob, carol, stranger]) {
      const ata = await createAssociatedTokenAccount(provider.connection, payer, mint, u.publicKey);
      await mintTo(provider.connection, payer, mint, ata, payer, 10_000n * T);
    }
    [pool] = findStakePoolPda(mint, program.programId);
    vault = findStakeVault(mint, pool);
  });

  it("init_config: only the upgrade authority; fees capped; only admin can change them", async () => {
    const init = (admin: anchor.web3.Keypair, launch: bigint, exit: bigint) =>
      program.methods.initConfig(attestor.publicKey, platform.publicKey, bn(launch), bn(exit)).accounts({ admin: admin.publicKey, programData: findProgramDataPda(program.programId) } as any).signers(admin === payer ? [] : [admin]).rpc();
    await refuse("a stranger created the config", () => init(stranger, LAUNCH_FEE, EXIT_FEE), /NotUpgradeAuthority/);
    await refuse("launch fee over the cap", () => init(payer, 200_000_001n, EXIT_FEE), /FeeTooHigh/);
    await refuse("exit fee over the cap", () => init(payer, LAUNCH_FEE, 50_000_001n), /FeeTooHigh/);
    await init(payer, LAUNCH_FEE, EXIT_FEE);
    const c: any = await program.account.config.fetch(config);
    assert.equal(c.admin.toBase58(), payer.publicKey.toBase58());
    assert.equal(c.attestor.toBase58(), attestor.publicKey.toBase58());
    assert.equal(c.feeReceiver.toBase58(), platform.publicKey.toBase58());
    assert.equal(c.launchFee.toString(), LAUNCH_FEE.toString());
    assert.equal(c.exitFee.toString(), EXIT_FEE.toString());
    await refuse("a stranger rotates the attestor", () => program.methods.setAttestor(stranger.publicKey).accounts({ admin: stranger.publicKey }).signers([stranger]).rpc(), /NotAdmin/);
    await refuse("a stranger sets the fees", () => program.methods.setFees(stranger.publicKey, bn(0), bn(0)).accounts({ admin: stranger.publicKey }).signers([stranger]).rpc(), /NotAdmin/);
    await refuse("admin sets a fee over the cap", () => program.methods.setFees(platform.publicKey, bn(LAUNCH_FEE), bn(50_000_001n)).accounts({ admin: payer.publicKey }).rpc(), /FeeTooHigh/);
    await refuse("admin sets no fee receiver", () => program.methods.setFees(PublicKey.default, bn(LAUNCH_FEE), bn(EXIT_FEE)).accounts({ admin: payer.publicKey }).rpc(), /BadFeeReceiver/);
    // admin can rotate and rotate back
    await program.methods.setAttestor(stranger.publicKey).accounts({ admin: payer.publicKey }).rpc();
    await program.methods.setAttestor(attestor.publicKey).accounts({ admin: payer.publicKey }).rpc();
    await program.methods.setFees(platform.publicKey, bn(LAUNCH_FEE), bn(EXIT_FEE)).accounts({ admin: payer.publicKey }).rpc();
    assert.equal(((await program.account.config.fetch(config)) as any).attestor.toBase58(), attestor.publicKey.toBase58());
  });

  it("init_pool (60 s windows, 60 s cooldown) pays the launch fee to the platform, and only to it", async () => {
    await refuse("launch fee sent elsewhere", () =>
      program.methods.initPool(new anchor.BN(EPOCH), new anchor.BN(EPOCH)).accounts({ creator: payer.publicKey, mint, bondingCurve: payer.publicKey, feeReceiver: stranger.publicKey, tokenProgram: TOKEN_PROGRAM_ID } as any).rpc(), /BadFeeReceiver/);
    const f0 = await balance(platform.publicKey);
    await initPool(mint, payer.publicKey, EPOCH, EPOCH); // test build: curve check skipped
    assert.equal((await balance(platform.publicKey)) - f0, LAUNCH_FEE, "the platform got the launch fee");
    const p = await poolAcc();
    assert.equal(p.epochSeconds.toNumber(), EPOCH);
    assert.equal(p.cooldownSeconds.toNumber(), EPOCH);
  });

  it("a locked coin (pump.fun creator = our PDA) ignores the parameters: always 1-min windows / 1-hour cooldown", async () => {
    const m = await createMint(provider.connection, payer, payer.publicKey, null, 6);
    const [creatorAuthority] = findCreatorAuthority(m, program.programId);
    await initPool(m, creatorAuthority, 3600, 86_400);
    const p: any = await program.account.stakePool.fetch(findStakePoolPda(m, program.programId)[0]);
    assert.equal(p.epochSeconds.toNumber(), 60, "a stranger opening the pool cannot pick the window");
    assert.equal(p.cooldownSeconds.toNumber(), 60 * 60, "nor the cooldown");
  });

  it("a legacy pool refuses windows over 1 h or cooldowns over 24 h", async () => {
    const m = await createMint(provider.connection, payer, payer.publicKey, null, 6);
    await refuse("window over 1 h", () => initPool(m, payer.publicKey, 3601, 60), /InvalidEpoch/);
    await refuse("cooldown over 24 h", () => initPool(m, payer.publicKey, 60, 86_401), /InvalidCooldown/);
  });

  it("SOL sent to a pool address before the pool exists is shared, not stranded", async () => {
    const m = await createMint(provider.connection, payer, payer.publicKey, null, 6);
    const [p] = findStakePoolPda(m, program.programId);
    await provider.sendAndConfirm(new Transaction().add(SystemProgram.transfer({ fromPubkey: payer.publicKey, toPubkey: p, lamports: 50_000_000 })), [payer]);
    await initPool(m, payer.publicKey, null, null);
    const acc: any = await program.account.stakePool.fetch(p);
    const info = await provider.connection.getAccountInfo(p);
    const rent = BigInt(await provider.connection.getMinimumBalanceForRentExemption(info!.data.length));
    assert.equal(acc.accountedLamports.toString(), rent.toString(), "only the rent is accounted");
    assert.equal(BigInt(info!.lamports), 50_000_000n);
    await program.methods.harvest().accounts({ pool: p }).rpc();
    const after: any = await program.account.stakePool.fetch(p);
    assert.equal(BigInt(after.carry.toString()) + BigInt(after.pendingRewards.toString()), 50_000_000n - rent, "everything above the rent is parked for the stakers");
  });

  it("a Token-2022 mint with a transfer fee cannot get a pool", async () => {
    const mintKp = Keypair.generate();
    const len = getMintLen([ExtensionType.TransferFeeConfig]);
    const lamports = await provider.connection.getMinimumBalanceForRentExemption(len);
    const tx = new Transaction().add(
      SystemProgram.createAccount({ fromPubkey: payer.publicKey, newAccountPubkey: mintKp.publicKey, space: len, lamports, programId: TOKEN_2022_PROGRAM_ID }),
      createInitializeTransferFeeConfigInstruction(mintKp.publicKey, payer.publicKey, payer.publicKey, 100, 1_000_000n, TOKEN_2022_PROGRAM_ID),
      createInitializeMintInstruction(mintKp.publicKey, 6, payer.publicKey, null, TOKEN_2022_PROGRAM_ID),
    );
    await provider.sendAndConfirm(tx, [payer, mintKp]);
    await refuse("a fee-on-transfer mint", () => initPool(mintKp.publicKey, payer.publicKey, null, null, TOKEN_2022_PROGRAM_ID), /UnsafeMint/);
  });

  it("two ways in: alice sends to the pool address (the attestor credits her); bob and carol sign `stake`", async () => {
    // --- transfer path
    const sigA = await sendToPool(alice, 5_000n * T);
    await refuse("a stranger attests", () => credit(alice.publicKey, 5_000n * T, sigA, stranger), /NotAttestor/);
    await refuse("credit beyond the vault", () => credit(alice.publicKey, 5_001n * T, sigA), /ExceedsVault/);
    await refuse("dust credited", () => credit(alice.publicKey, 10n * T, sigA), /DepositTooSmall/);
    // --- wallet path, while alice's transfer is still waiting for its credit
    await refuse("stake of 0", () => stake(bob, 0n), /ZeroAmount/);
    await refuse("bob stakes from alice's token account", () => stake(bob, 100n * T, findUserAta(mint, alice.publicKey)), anyErr);
    const b0 = await balance(bob.publicKey);
    await stake(bob, 5_000n * T);
    const rentBob = BigInt((await provider.connection.getAccountInfo(position(bob.publicKey)))!.lamports);
    assert.ok(b0 - (await balance(bob.publicKey)) >= rentBob, "bob paid the rent of his own position");
    await stake(carol, 2_000n * T);
    await stake(carol, 500n * T); // adding to a position
    assert.equal((await getAccount(provider.connection, vault)).amount, 12_500n * T);
    // the wallet stakes did not eat the room of alice's uncredited transfer, and nothing more can be attributed
    await refuse("credit beyond what was sent", () => credit(stranger.publicKey, 5_001n * T, sigA), /ExceedsVault/);
    await credit(alice.publicKey, 5_000n * T, sigA);
    await refuse("nothing left to attribute", () => credit(stranger.publicKey, 1_000n * T, sigA), /ExceedsVault/);

    const [pa, pb, pc, p] = [await pos(alice.publicKey), await pos(bob.publicKey), await pos(carol.publicKey), await poolAcc()];
    assert.equal(BigInt(pa.pending.toString()), 5_000n * T);
    assert.equal(BigInt(pb.pending.toString()), 5_000n * T);
    assert.equal(BigInt(pc.pending.toString()), 2_500n * T);
    assert.ok(pa.rentPayer.equals(attestor.publicKey), "the attestor paid alice's position");
    assert.ok(pb.rentPayer.equals(bob.publicKey), "bob paid his");
    assert.equal(BigInt(p.vaultAccounted.toString()), 12_500n * T, "everything in the vault is attributed");
    assert.equal(p.stakerCount, 3);
  });

  it("attacks: every way to move someone else's tokens or SOL is refused (no state changes)", async () => {
    const before = { pool: await poolAcc(), vault: (await getAccount(provider.connection, vault)).amount };
    const sig = (await provider.connection.getSignaturesForAddress(vault, { limit: 1 }, "confirmed"))[0].signature;
    await refuse("credit the pool itself", () => credit(pool, 1_000n * T, sig), /BadOwner/);
    await refuse("credit the zero address", () => credit(PublicKey.default, 1_000n * T, sig), /BadOwner/);
    await refuse("stranger credits himself", () => credit(stranger.publicKey, 1_000n * T, sig, stranger), /NotAttestor|ConstraintHasOne/);
    await refuse("stranger makes alice leave (attested path)", () => requestExit(alice.publicKey, sig, stranger), /NotAttestor|ConstraintHasOne/);
    await refuse("stranger makes bob leave (wallet path)", () => requestUnstake(stranger, ALL, bob.publicKey), anyErr);
    await refuse("exit for a wallet with no position", () => requestExit(stranger.publicKey, sig), anyErr);
    await refuse("exit fee sent elsewhere", () =>
      program.methods.requestUnstake(ALL).accounts({ owner: bob.publicKey, feeReceiver: stranger.publicKey, pool, position: position(bob.publicKey) } as any).signers([bob]).rpc(), /BadFeeReceiver/);
    await refuse("alice's payout to the stranger", () =>
      program.methods.claim(new anchor.BN(0)).accounts({ payer: stranger.publicKey, owner: stranger.publicKey, pool, position: position(alice.publicKey) } as any).signers([stranger]).rpc(), anyErr);
    await refuse("alice's tokens to the stranger", () =>
      program.methods.unstake().accounts({ payer: stranger.publicKey, owner: stranger.publicKey, mint, pool, position: position(alice.publicKey), tokenVault: vault, ownerTokenAccount: findUserAta(mint, stranger.publicKey), tokenProgram: TOKEN_PROGRAM_ID } as any).signers([stranger]).rpc(), anyErr);
    await refuse("alice's tokens before she asked to leave", () => unstakeFor(alice.publicKey, stranger), /NothingToUnstake|CooldownActive|0x/);
    await refuse("close bob's position to the stranger", () => closeFor(bob.publicKey, stranger.publicKey), /NotRentPayer|ConstraintAddress/);
    await refuse("close a position that still holds tokens", () => closeFor(alice.publicKey, attestor.publicKey), /PositionNotEmpty/);
    const other = await createMint(provider.connection, payer, payer.publicKey, null, 6);
    await refuse("credit backed by another coin's vault", () =>
      program.methods.creditDeposit(bn(1_000n * T), sigBytes(sig)).accounts({ attestor: attestor.publicKey, owner: alice.publicKey, mint: other, pool, tokenVault: vault, tokenProgram: TOKEN_PROGRAM_ID } as any).signers([attestor]).rpc(), anyErr);
    await refuse("payer keeps more than 5,000 lamports", () => claimFor(alice.publicKey, 5_001, stranger), /ClaimFeeTooHigh/);
    const after = { pool: await poolAcc(), vault: (await getAccount(provider.connection, vault)).amount };
    assert.equal(after.vault, before.vault, "vault untouched");
    assert.equal(after.pool.totalPending.toString(), before.pool.totalPending.toString(), "pending untouched");
    assert.equal(after.pool.totalActive.toString(), before.pool.totalActive.toString(), "active untouched");
    assert.equal(after.pool.stakerCount, before.pool.stakerCount, "stakers untouched");
    assert.equal(after.pool.vaultAccounted.toString(), before.pool.vaultAccounted.toString(), "credits untouched");
  });

  it("fees of the current window are NOT paid to deposits made during it", async () => {
    await simulateFees(1);
    await nextBoundary(); // closes the window in which the three staked
    await harvest();
    const p = await poolAcc();
    assert.equal(BigInt(p.totalActive.toString()), 12_500n * T, "all active now, whatever the path");
    assert.equal(BigInt(p.pendingRewards.toString()), 1n * BigInt(LAMPORTS_PER_SOL), "held: nobody was active for that window");
  });

  it("next window: 2.5 SOL split pro rata (40 / 40 / 20 %): pushed by anyone, or pulled by the owner", async () => {
    await simulateFees(1.5);
    await nextBoundary();
    const [a0, b0, c0] = [await balance(alice.publicKey), await balance(bob.publicKey), await balance(carol.publicKey)];
    await claimFor(alice.publicKey); // the crank pays the signature; alice signs nothing, ever
    await claimFor(bob.publicKey);
    await program.methods.claim(new anchor.BN(5_000)).accounts({ payer: carol.publicKey, owner: carol.publicKey, pool, position: position(carol.publicKey) }).signers([carol]).rpc(); // carol pulls her own: no crank needed, and no fee kept
    const SOL = BigInt(LAMPORTS_PER_SOL);
    assert.equal((await balance(alice.publicKey)) - a0, SOL);
    assert.equal((await balance(bob.publicKey)) - b0, SOL);
    assert.equal((await balance(carol.publicKey)) - c0, SOL / 2n, "everything owed: an owner pulling its own payout keeps no fee from it");
  });

  it("wallet way out: bob signs, pays the exit fee to the platform, and takes his coins back himself after the cooldown (no crank, no attestor)", async () => {
    const [f0, b0, p0] = [await balance(platform.publicKey), await balance(bob.publicKey), await balance(pool)];
    await requestUnstake(bob, bn(2_000n * T)); // part of it
    assert.equal((await balance(platform.publicKey)) - f0, EXIT_FEE, "exit fee to the platform");
    assert.equal(b0 - (await balance(bob.publicKey)), EXIT_FEE, "bob paid the exit fee, nothing else");
    assert.equal(await balance(pool), p0, "the pool's SOL is untouched");
    let pb = await pos(bob.publicKey);
    assert.equal(BigInt(pb.cooldownAmount.toString()), 2_000n * T);
    assert.equal(BigInt(pb.amount.toString()), 3_000n * T, "the rest keeps earning");
    await refuse("more than he has", () => requestUnstake(bob, bn(3_001n * T)), /InsufficientStake/);
    await requestUnstake(bob, ALL); // then everything
    pb = await pos(bob.publicKey);
    assert.equal(BigInt(pb.cooldownAmount.toString()), 5_000n * T);
    assert.equal(BigInt(pb.amount.toString()), 0n);
    assert.equal((await balance(platform.publicKey)) - f0, 2n * EXIT_FEE);
    await refuse("nothing left to unstake", () => requestUnstake(bob, ALL), /NothingStaked/);
    await refuse("cooldown not elapsed", () => unstakeFor(bob.publicKey, bob), /CooldownActive/);
    await sleep((EPOCH + 3) * 1000);
    await unstakeFor(bob.publicKey, bob); // bob himself
    assert.equal((await getAccount(provider.connection, findUserAta(mint, bob.publicKey))).amount, 10_000n * T, "bob has all his tokens back");
    const p = await poolAcc();
    assert.equal(BigInt(p.vaultAccounted.toString()), 7_500n * T, "alice's and carol's tokens remain attributed");
    assert.equal((await getAccount(provider.connection, vault)).amount, 7_500n * T);
  });

  it("transfer way out: alice sends 0.01 SOL to the pool address; attested in the same window, the platform gets the exit fee and the stakers' fees are untouched", async () => {
    await harvest();
    const carry0 = BigInt((await poolAcc()).carry.toString());
    const f0 = await balance(platform.publicKey);
    const sig = await provider.sendAndConfirm(new Transaction().add(SystemProgram.transfer({ fromPubkey: alice.publicKey, toPubkey: pool, lamports: 10_000_000 })), [alice]);
    await refuse("a stranger attests the exit", () => requestExit(alice.publicKey, sig, stranger), /NotAttestor/);
    await refuse("exit fee sent elsewhere", () =>
      program.methods.requestExit(sigBytes(sig)).accounts({ attestor: attestor.publicKey, feeReceiver: stranger.publicKey, owner: alice.publicKey, pool, position: position(alice.publicKey) } as any).signers([attestor]).rpc(), /BadFeeReceiver/);
    await requestExit(alice.publicKey, sig);
    assert.equal((await balance(platform.publicKey)) - f0, EXIT_FEE, "the platform got the exit fee");
    const p = await poolAcc();
    assert.equal(BigInt(p.carry.toString()), carry0, "the window's fees are exactly what they were before the 0.01 SOL");
    const pa = await pos(alice.publicKey);
    assert.equal(BigInt(pa.cooldownAmount.toString()), 5_000n * T, "everything cooling down");
    await refuse("nothing left to exit", () => requestExit(alice.publicKey, sig), /NothingStaked/);
    const info = (await provider.connection.getAccountInfo(pool))!;
    assert.ok(info.lamports >= (await provider.connection.getMinimumBalanceForRentExemption(info.data.length)), "pool stays rent-exempt");
  });

  it("an exit attested after its window closed costs the stakers nothing: the 0.01 SOL was already theirs, the platform takes nothing", async () => {
    await stake(stranger, 2_500n * T); // a second staker next to carol
    await nextBoundary();
    await harvest(); // stranger active from here
    const sig = await provider.sendAndConfirm(new Transaction().add(SystemProgram.transfer({ fromPubkey: stranger.publicKey, toPubkey: pool, lamports: 10_000_000 })), [stranger]);
    await nextBoundary(); // the window that received the 0.01 SOL closes before the attestor acts
    await harvest();
    const [f0, c0] = [await balance(platform.publicKey), await balance(carol.publicKey)];
    await requestExit(stranger.publicKey, sig);
    assert.equal((await balance(platform.publicKey)) - f0, 0n, "nothing left in the running window: no fee");
    await claimFor(carol.publicKey);
    assert.equal((await balance(carol.publicKey)) - c0, 5_000_000n, "carol (half the active stake) got half of that 0.01 SOL");
  });

  it("stray SOL sent to the pool is paid to the stakers who stay; the caller of a payout keeps at most its signature fee", async () => {
    await provider.sendAndConfirm(new Transaction().add(SystemProgram.transfer({ fromPubkey: payer.publicKey, toPubkey: pool, lamports: 50_000_000 })), [payer]);
    await nextBoundary();
    const [c0, s0] = [await balance(carol.publicKey), await balance(bob.publicKey)];
    await refuse("fee over the cap", () => claimFor(carol.publicKey, 5_001), /ClaimFeeTooHigh/);
    await program.methods.claim(new anchor.BN(5_000)).accounts({ payer: bob.publicKey, owner: carol.publicKey, pool, position: position(carol.publicKey) }).signers([bob]).rpc();
    const got = (await balance(carol.publicKey)) - c0;
    assert.ok(got >= 49_994_000n && got <= 49_995_000n, `carol, the only active staker, got ${got}`);
    assert.equal((await balance(bob.publicKey)) - s0, 5_000n, "the caller keeps one signature's fee, no more");
    assert.ok(BigInt((await pos(carol.publicKey)).totalClaimed.toString()) > 0n);
    await refuse("claim with nothing owed", () => claimFor(bob.publicKey), /NothingToClaim/);
  });

  it("close_position: only when empty; the rent goes back to whoever paid it (bob himself, the attestor for alice)", async () => {
    const count = async () => (await poolAcc()).stakerCount;
    await refuse("a staking position was closed", () => closeFor(carol.publicKey, carol.publicKey), /PositionNotEmpty/);
    await refuse("bob's rent sent to the attestor", () => closeFor(bob.publicKey, attestor.publicKey), /NotRentPayer/);
    const c0 = await count();
    const b0 = await balance(bob.publicKey);
    const rent = BigInt((await provider.connection.getAccountInfo(position(bob.publicKey)))!.lamports);
    await closeFor(bob.publicKey, bob.publicKey); // anyone (here the provider wallet) can close it
    assert.isNull(await provider.connection.getAccountInfo(position(bob.publicKey)), "closed");
    assert.equal((await balance(bob.publicKey)) - b0, rent, "bob, who paid the rent, gets it back");
    assert.equal(await count(), c0 - 1);
    // alice: cooldown elapsed long ago; anyone returns her tokens, then closes; the attestor gets its rent back
    await unstakeFor(alice.publicKey);
    assert.equal((await getAccount(provider.connection, findUserAta(mint, alice.publicKey))).amount, 10_000n * T, "alice has all her tokens back");
    await refuse("alice's rent sent to alice", () => closeFor(alice.publicKey, alice.publicKey), /NotRentPayer/);
    const r0 = await balance(attestor.publicKey);
    const rentA = BigInt((await provider.connection.getAccountInfo(position(alice.publicKey)))!.lamports);
    await closeFor(alice.publicKey, attestor.publicKey);
    assert.equal((await balance(attestor.publicKey)) - r0, rentA, "the attestor, who paid alice's rent, gets it back");
    // bob stakes again: a fresh position, counted again
    await stake(bob, 1_000n * T);
    const pb = await pos(bob.publicKey);
    assert.equal(BigInt(pb.pending.toString()), 1_000n * T);
    assert.equal(BigInt(pb.totalClaimed.toString()), 0n);
    assert.equal(await count(), c0 - 1);
  });

  it("rescue_token: the admin sends back a token sent to a pool by mistake; never the pool's own coin, never by a stranger", async () => {
    const stray = await createMint(provider.connection, payer, payer.publicKey, null, 6);
    const strayAtPool = (await getOrCreateAssociatedTokenAccount(provider.connection, payer, stray, pool, true)).address;
    const back = (await getOrCreateAssociatedTokenAccount(provider.connection, payer, stray, alice.publicKey)).address;
    await mintTo(provider.connection, payer, stray, strayAtPool, payer, 777n * T);
    const rescue = (admin: anchor.web3.Keypair, strayMint: PublicKey, from: PublicKey, to: PublicKey) =>
      program.methods.rescueToken().accounts({ admin: admin.publicKey, pool, strayMint, from, to, tokenProgram: TOKEN_PROGRAM_ID } as any).signers(admin === payer ? [] : [admin]).rpc();
    await refuse("a stranger rescues", () => rescue(stranger, stray, strayAtPool, back), /NotAdmin/);
    await refuse("the admin takes the stakers' coin", () => rescue(payer, mint, vault, findUserAta(mint, alice.publicKey)), /NotAStrayToken/);
    await rescue(payer, stray, strayAtPool, back);
    assert.equal((await getAccount(provider.connection, back)).amount, 777n * T);
    assert.equal((await getAccount(provider.connection, vault)).amount, 6_000n * T, "the vault is untouched (carol 2,500 + stranger 2,500 cooling down + bob 1,000)");
  });

  it("lock_fee_split accepts exactly 100 % to the pool, nothing else", async () => {
    const PUMP_FEE = new PublicKey("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
    const DISC = Buffer.from([111, 251, 49, 6, 78, 78, 106, 18]);
    const encode = (shares: Array<[PublicKey, number]>) => {
      const b = Buffer.alloc(8 + 4 + shares.length * 34);
      DISC.copy(b, 0);
      b.writeUInt32LE(shares.length, 8);
      shares.forEach(([k, bps], i) => { k.toBuffer().copy(b, 12 + i * 34); b.writeUInt16LE(bps, 12 + i * 34 + 32); });
      return b;
    };
    type Key = { pubkey: PublicKey; isWritable: boolean; isSigner: boolean };
    const attempt = (m: PublicKey, data: Buffer, keys: Key[] = []) =>
      program.methods.lockFeeSplit(0, Buffer.alloc(0), data, new anchor.BN(0)).accounts({ payer: payer.publicKey, mint: m, feeProgram: PUMP_FEE } as any).remainingAccounts(keys).rpc();
    /** Passes our validation and reaches the CPI (there is no fee program on the local validator). */
    const reachesCpi = async (what: string, f: () => Promise<unknown>) => {
      try { await f(); assert.fail(`${what}: no fee program here, the CPI must fail`); } catch (e: any) { assert.notMatch(e.toString(), /BadSplit|BadCpi/, what); }
    };

    // 100 % to the pool, nothing else
    await refuse("100 % to the creator wallet", () => attempt(mint, encode([[payer.publicKey, 10_000]])), /BadSplit/);
    await refuse("a launcher share", () => attempt(mint, encode([[pool, 8_500], [payer.publicKey, 1_500]])), /BadSplit/);
    await refuse("a second share at 0 %", () => attempt(mint, encode([[pool, 10_000], [payer.publicKey, 0]])), /BadSplit/);
    await refuse("wrong instruction", () => attempt(mint, Buffer.concat([Buffer.from([1, 2, 3, 4, 5, 6, 7, 8]), encode([[pool, 10_000]]).subarray(8)])), /BadCpi/);
    await refuse("the caller lends its own signature to the fee program", () => attempt(mint, encode([[pool, 10_000]]), [{ pubkey: payer.publicKey, isWritable: true, isSigner: true }]), /BadCpi/);
    await reachesCpi("100 % to the pool", () => attempt(mint, encode([[pool, 10_000]])));
  });
});
