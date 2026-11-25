// darkpool-wrap functional and adversarial tests against a local solana-test-validator.
// Run via ./run.sh (starts the validator with target/deploy/darkpool_wrap.so loaded).
//
// Three source mints are exercised end to end:
//   1. Token-2022 mint shaped like an xStock (MetadataPointer, TokenMetadata, PermanentDelegate,
//      DefaultAccountState, ScaledUiAmount, Pausable, TransferHook, freeze authority),
//      with the wrapped-mint PDA pre-funded by an attacker (grief).
//   2. Token-2022 mint with a 1% transfer fee and no freeze authority.
//   3. Classic SPL Token mint without metadata.
import {
  Connection, Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction,
  ComputeBudgetProgram, sendAndConfirmTransaction, LAMPORTS_PER_SOL,
} from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID as T22, ASSOCIATED_TOKEN_PROGRAM_ID, ExtensionType, getMintLen, AccountState,
  createInitializeMintInstruction, createInitializeMetadataPointerInstruction, createInitializePermanentDelegateInstruction,
  createInitializeDefaultAccountStateInstruction, createInitializeScaledUiAmountConfigInstruction,
  createInitializePausableConfigInstruction, createInitializeTransferHookInstruction,
  createInitializeTransferFeeConfigInstruction, createMintToInstruction, getMint, getExtensionTypes,
  getTokenMetadata, getAssociatedTokenAddressSync, MINT_SIZE,
} from '@solana/spl-token';
import { createInitializeInstruction as createInitMetadata, pack as packMeta } from '@solana/spl-token-metadata';

const RPC = process.env.RPC_URL || 'http://127.0.0.1:8899';
const PROGRAM = new PublicKey(process.env.PROGRAM_ID || '4WMGnz33sRKHynF2an3kWmfZtd8eudTVCExtSmMWuaCc');
const conn = new Connection(RPC, 'confirmed');
const VERBOSE = !!process.env.VERBOSE;

// ------------------------------------------------------------------ client helpers
const meta = (pubkey, isWritable = false, isSigner = false) => ({ pubkey, isWritable, isSigner });
const u64 = (n) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(n)); return b; };

function pdas(unwrappedMint, unwrappedTokenProgram) {
  const [wrappedMint] = PublicKey.findProgramAddressSync([Buffer.from('mint'), unwrappedMint.toBuffer(), T22.toBuffer()], PROGRAM);
  const [authority] = PublicKey.findProgramAddressSync([Buffer.from('authority'), wrappedMint.toBuffer()], PROGRAM);
  const escrow = getAssociatedTokenAddressSync(unwrappedMint, authority, true, unwrappedTokenProgram);
  return { wrappedMint, authority, escrow, unwrappedMint, unwrappedTokenProgram };
}

const ixCreateWrappedMint = (p, payer) => new TransactionInstruction({
  programId: PROGRAM,
  keys: [meta(payer, true, true), meta(p.wrappedMint, true), meta(p.authority), meta(p.unwrappedMint), meta(SystemProgram.programId), meta(T22)],
  data: Buffer.from([0]),
});

const ixWrap = (p, { recipientWrapped, unwrappedAccount, owner, amount }) => new TransactionInstruction({
  programId: PROGRAM,
  keys: [meta(recipientWrapped, true), meta(p.wrappedMint, true), meta(p.authority), meta(p.unwrappedTokenProgram), meta(T22),
    meta(unwrappedAccount, true), meta(p.unwrappedMint), meta(p.escrow, true), meta(owner, false, true)],
  data: Buffer.concat([Buffer.from([1]), u64(amount)]),
});

const ixUnwrap = (p, { recipientUnwrapped, wrappedAccount, owner, amount }) => new TransactionInstruction({
  programId: PROGRAM,
  keys: [meta(p.escrow, true), meta(recipientUnwrapped, true), meta(p.authority), meta(p.unwrappedMint), meta(T22),
    meta(p.unwrappedTokenProgram), meta(wrappedAccount, true), meta(p.wrappedMint, true), meta(owner, false, true)],
  data: Buffer.concat([Buffer.from([2]), u64(amount)]),
});

function ataIdempotent(payer, owner, mint, tokenProgram) {
  const ata = getAssociatedTokenAddressSync(mint, owner, true, tokenProgram);
  const ix = new TransactionInstruction({
    programId: ASSOCIATED_TOKEN_PROGRAM_ID,
    keys: [meta(payer, true, true), meta(ata, true), meta(owner), meta(mint), meta(SystemProgram.programId), meta(tokenProgram)],
    data: Buffer.from([1]),
  });
  return { ata, ix };
}

async function send(ixs, signers, label = '') {
  const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 400_000 }), ...ixs);
  const sig = await sendAndConfirmTransaction(conn, tx, signers, { commitment: 'confirmed' });
  if (VERBOSE) console.log(`    tx ${label} ${sig}`);
  return sig;
}

// ------------------------------------------------------------------ tiny test harness
let pass = 0, fail = 0;
const ok = (cond, msg) => { cond ? pass++ : fail++; console.log(`  ${cond ? 'ok  ' : 'FAIL'} ${msg}`); };
async function mustFail(fn, msg) {
  try { await fn(); ok(false, `${msg} (expected rejection)`); }
  catch (e) { ok(true, `${msg} -> rejected (${String(e.message).match(/custom program error: 0x\w+|Error: [^\n]{0,50}/)?.[0] ?? 'error'})`); }
}
const balance = async (a) => BigInt((await conn.getTokenAccountBalance(a)).value.amount);
async function airdrop(pubkey, sol) { await conn.confirmTransaction(await conn.requestAirdrop(pubkey, sol * LAMPORTS_PER_SOL), 'confirmed'); }

// ------------------------------------------------------------------ fixtures
const payer = Keypair.generate();
const DECIMALS = 8;

async function xstockLikeMint({ freeze = true, fee = false } = {}) {
  const kp = Keypair.generate(); const mint = kp.publicKey;
  const exts = [ExtensionType.MetadataPointer, ExtensionType.PermanentDelegate, ExtensionType.DefaultAccountState,
    ExtensionType.ScaledUiAmountConfig, ExtensionType.PausableConfig, ExtensionType.TransferHook,
    ...(fee ? [ExtensionType.TransferFeeConfig] : [])];
  const space = getMintLen(exts);
  const md = { mint, updateAuthority: payer.publicKey, name: 'Test NVIDIA xStock', symbol: 'tNVDAx', uri: 'https://example.com/tnvdax.json', additionalMetadata: [] };
  const lamports = await conn.getMinimumBalanceForRentExemption(space + 4 + packMeta(md).length);
  await send([
    SystemProgram.createAccount({ fromPubkey: payer.publicKey, newAccountPubkey: mint, space, lamports, programId: T22 }),
    createInitializeMetadataPointerInstruction(mint, payer.publicKey, mint, T22),
    createInitializePermanentDelegateInstruction(mint, payer.publicKey, T22),
    createInitializeDefaultAccountStateInstruction(mint, AccountState.Initialized, T22),
    createInitializeScaledUiAmountConfigInstruction(mint, payer.publicKey, 1.0009, T22),
    createInitializePausableConfigInstruction(mint, payer.publicKey, T22),
    createInitializeTransferHookInstruction(mint, payer.publicKey, PublicKey.default, T22),
    ...(fee ? [createInitializeTransferFeeConfigInstruction(mint, payer.publicKey, payer.publicKey, 100, 1_000_000n, T22)] : []),
    createInitializeMintInstruction(mint, DECIMALS, payer.publicKey, freeze ? payer.publicKey : null, T22),
    createInitMetadata({ programId: T22, metadata: mint, updateAuthority: payer.publicKey, mint, mintAuthority: payer.publicKey, name: md.name, symbol: md.symbol, uri: md.uri }),
  ], [payer, kp], 'source mint');
  return mint;
}

async function classicSplMint() {
  const kp = Keypair.generate();
  await send([
    SystemProgram.createAccount({ fromPubkey: payer.publicKey, newAccountPubkey: kp.publicKey, space: MINT_SIZE, lamports: await conn.getMinimumBalanceForRentExemption(MINT_SIZE), programId: TOKEN_PROGRAM_ID }),
    createInitializeMintInstruction(kp.publicKey, 6, payer.publicKey, null, TOKEN_PROGRAM_ID),
  ], [payer, kp], 'classic SPL mint');
  return kp.publicKey;
}

// ------------------------------------------------------------------ scenario
async function scenario(mint, tokenProgram, label, { grief = false, fee = false } = {}) {
  console.log(`\n# ${label}`);
  const p = pdas(mint, tokenProgram);
  const src = ataIdempotent(payer.publicKey, payer.publicKey, mint, tokenProgram);
  await send([src.ix, createMintToInstruction(mint, src.ata, payer.publicKey, 1000n * 10n ** 8n, [], tokenProgram)], [payer], 'fund user');
  if (grief) {
    // Anyone can send lamports to the PDA before it exists; creation must still succeed.
    await send([SystemProgram.transfer({ fromPubkey: payer.publicKey, toPubkey: p.wrappedMint, lamports: await conn.getMinimumBalanceForRentExemption(0) })], [payer], 'pre-fund PDA');
  }

  // create_wrapped_mint
  await send([ixCreateWrappedMint(p, payer.publicKey)], [payer], 'create_wrapped_mint');
  const wm = await getMint(conn, p.wrappedMint, 'confirmed', T22);
  const sm = await getMint(conn, mint, 'confirmed', tokenProgram);
  const exts = getExtensionTypes(wm.tlvData).map((e) => ExtensionType[e]);
  ok(exts.every((e) => ['MetadataPointer', 'TokenMetadata'].includes(e)), `wrapped mint carries only MetadataPointer/TokenMetadata [${exts}]`);
  ok(wm.decimals === sm.decimals, `decimals copied (${wm.decimals})`);
  ok(String(wm.freezeAuthority) === String(sm.freezeAuthority), `freeze authority copied (${wm.freezeAuthority ?? 'none'})`);
  ok(wm.mintAuthority.equals(p.authority), 'mint authority is the authority PDA');
  if (tokenProgram.equals(T22)) {
    const md = await getTokenMetadata(conn, p.wrappedMint, 'confirmed', T22);
    ok(md?.name === 'Test NVIDIA xStock' && md.symbol === 'tNVDAx' && md.uri.endsWith('tnvdax.json'), `metadata copied on-chain (${md?.name} / ${md?.symbol})`);
  }
  await mustFail(() => send([ixCreateWrappedMint(p, payer.publicKey)], [payer]), 'second create_wrapped_mint');

  // wrap / unwrap
  const escrow = ataIdempotent(payer.publicKey, p.authority, mint, tokenProgram);
  const dst = ataIdempotent(payer.publicKey, payer.publicKey, p.wrappedMint, T22);
  const amount = 10n * 10n ** 8n;
  await send([escrow.ix, dst.ix, ixWrap(p, { recipientWrapped: dst.ata, unwrappedAccount: src.ata, owner: payer.publicKey, amount })], [payer], 'wrap');
  const escrowBal = await balance(escrow.ata), wrappedBal = await balance(dst.ata);
  ok(wrappedBal === escrowBal, `1:1 backing: wrapped supply ${wrappedBal} == escrow ${escrowBal}`);
  ok(fee ? wrappedBal < amount : wrappedBal === amount, fee ? 'transfer fee: minted only what escrow received' : 'wrapped amount exact');
  const out = 4n * 10n ** 8n;
  await send([ixUnwrap(p, { recipientUnwrapped: src.ata, wrappedAccount: dst.ata, owner: payer.publicKey, amount: out })], [payer], 'unwrap');
  ok(await balance(dst.ata) === wrappedBal - out && await balance(escrow.ata) === escrowBal - out, 'unwrap burns and releases 1:1');

  // adversarial
  await mustFail(() => send([ixUnwrap(p, { recipientUnwrapped: src.ata, wrappedAccount: dst.ata, owner: payer.publicKey, amount: 10n ** 12n })], [payer]), 'unwrap more than held');
  await mustFail(() => send([ixWrap(p, { recipientWrapped: dst.ata, unwrappedAccount: src.ata, owner: payer.publicKey, amount: 0 })], [payer]), 'wrap zero');
  await mustFail(() => send([ixWrap({ ...p, escrow: src.ata }, { recipientWrapped: dst.ata, unwrappedAccount: src.ata, owner: payer.publicKey, amount: 1 })], [payer]), 'wrap into a user-owned "escrow"');
  const thief = Keypair.generate();
  await airdrop(thief.publicKey, 1);
  await mustFail(() => send([ixUnwrap(p, { recipientUnwrapped: src.ata, wrappedAccount: dst.ata, owner: thief.publicKey, amount: 1 })], [thief]), 'unwrap by non-owner');
  await mustFail(() => send([ixUnwrap({ ...p, wrappedMint: Keypair.generate().publicKey }, { recipientUnwrapped: src.ata, wrappedAccount: dst.ata, owner: payer.publicKey, amount: 1 })], [payer]), 'unwrap with a fake wrapped mint');
  await mustFail(() => send([ixWrap({ ...p, authority: payer.publicKey }, { recipientWrapped: dst.ata, unwrappedAccount: src.ata, owner: payer.publicKey, amount: 1 })], [payer]), 'wrap with a wrong authority');
}

console.log(`darkpool-wrap tests: program ${PROGRAM.toBase58()} on ${RPC}`);
await airdrop(payer.publicKey, 20);
await scenario(await xstockLikeMint(), T22, 'xStock-like Token-2022 mint (all extensions, freeze authority, griefed PDA)', { grief: true });
await scenario(await xstockLikeMint({ freeze: false, fee: true }), T22, 'Token-2022 mint with transfer fee, no freeze authority', { fee: true });
await scenario(await classicSplMint(), TOKEN_PROGRAM_ID, 'classic SPL Token mint, no metadata');
console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
