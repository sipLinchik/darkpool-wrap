//! darkpool-wrap: minimal 1:1 wrapper of any SPL Token / Token-2022 mint into a
//! plain Token-2022 mint (MetadataPointer + TokenMetadata only).
//!
//! Same PDA scheme and account order as SPL token-wrap
//! (github.com/solana-program/token-wrap), reduced to three instructions:
//!
//! 0 `CreateWrappedMint`
//!   0. `[ws]` payer
//!   1. `[w]`  wrapped mint, PDA `["mint", unwrapped_mint, token_2022]`
//!   2. `[]`   wrapped mint authority, PDA `["authority", wrapped_mint]`
//!   3. `[]`   unwrapped mint
//!   4. `[]`   system program
//!   5. `[]`   Token-2022 program
//!   Decimals and freeze authority are copied from the unwrapped mint. If the
//!   unwrapped mint carries an in-mint TokenMetadata, name/symbol/uri are copied
//!   on-chain (no caller-supplied metadata, so the PDA cannot be squatted with a
//!   fake name). Update authority is the mint authority PDA, which exposes no
//!   update path: the metadata is immutable.
//!
//! 1 `Wrap { amount: u64 }`
//!   0. `[w]` recipient wrapped token account
//!   1. `[w]` wrapped mint
//!   2. `[]`  wrapped mint authority
//!   3. `[]`  unwrapped token program
//!   4. `[]`  Token-2022 program (wrapped)
//!   5. `[w]` source unwrapped token account
//!   6. `[]`  unwrapped mint
//!   7. `[w]` escrow: unwrapped token account owned by the authority PDA
//!   8. `[s]` owner/delegate of the source account
//!   9.. extra accounts forwarded to transfer_checked (transfer hook)
//!   Mints exactly the amount the escrow received, so a transfer fee on the
//!   unwrapped mint can never break 1:1 backing.
//!
//! 2 `Unwrap { amount: u64 }`
//!   0. `[w]` escrow
//!   1. `[w]` recipient unwrapped token account
//!   2. `[]`  wrapped mint authority
//!   3. `[]`  unwrapped mint
//!   4. `[]`  Token-2022 program (wrapped)
//!   5. `[]`  unwrapped token program
//!   6. `[w]` source wrapped token account
//!   7. `[w]` wrapped mint
//!   8. `[s]` owner/delegate of the source wrapped account
//!   9.. extra accounts forwarded to transfer_checked (transfer hook)
#![no_std]

use pinocchio::{
    cpi::{invoke_signed_with_bounds, Seed, Signer},
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    sysvars::{rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};

#[cfg(not(feature = "no-entrypoint"))]
mod entry {
    pinocchio::program_entrypoint!(crate::process_instruction);
    pinocchio::no_allocator!();
    pinocchio::nostd_panic_handler!();
}

pub const TOKEN_PROGRAM: Address = solana_address::address!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const TOKEN_2022_PROGRAM: Address =
    solana_address::address!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
pub const SYSTEM_PROGRAM: Address = solana_address::address!("11111111111111111111111111111111");

pub const MINT_SEED: &[u8] = b"mint";
pub const AUTHORITY_SEED: &[u8] = b"authority";

/// Base mint (82) + padding to 165 + account type (1) + MetadataPointer TLV (4 + 64).
const WRAPPED_MINT_BASE_LEN: usize = 234;
const MINT_BASE_LEN: usize = 82;
const ACCOUNT_LEN: usize = 165;
const EXT_TOKEN_METADATA: u16 = 19;
const TOKEN_METADATA_INIT: [u8; 8] = [0xd2, 0xe1, 0x1e, 0xa2, 0x58, 0xb8, 0x4d, 0x8d];
const MAX_CPI_ACCOUNTS: usize = 24;

#[repr(u32)]
pub enum WrapError {
    WrappedMintMismatch = 1,
    AuthorityMismatch = 2,
    BadTokenProgram = 3,
    BadEscrow = 4,
    ZeroAmount = 5,
    AlreadyCreated = 6,
    BadMint = 7,
}

impl From<WrapError> for ProgramError {
    fn from(e: WrapError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

pub fn process_instruction(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    match data.split_first() {
        Some((0, _)) => create_wrapped_mint(program_id, accounts),
        Some((1, rest)) => wrap(program_id, accounts, amount(rest)?),
        Some((2, rest)) => unwrap(program_id, accounts, amount(rest)?),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn amount(rest: &[u8]) -> Result<u64, ProgramError> {
    let bytes: [u8; 8] = rest
        .get(..8)
        .and_then(|b| b.try_into().ok())
        .ok_or(ProgramError::InvalidInstructionData)?;
    match u64::from_le_bytes(bytes) {
        0 => Err(WrapError::ZeroAmount.into()),
        a => Ok(a),
    }
}

/// Returns the wrapped mint PDA bump after checking `wrapped_mint` matches.
fn check_wrapped_mint(program_id: &Address, wrapped_mint: &AccountView, unwrapped_mint: &AccountView) -> Result<u8, ProgramError> {
    let (addr, bump) = Address::find_program_address(
        &[MINT_SEED, unwrapped_mint.address().as_ref(), TOKEN_2022_PROGRAM.as_ref()],
        program_id,
    );
    if &addr != wrapped_mint.address() {
        return Err(WrapError::WrappedMintMismatch.into());
    }
    Ok(bump)
}

/// Returns the authority PDA bump after checking `authority` matches.
fn check_authority(program_id: &Address, authority: &AccountView, wrapped_mint: &AccountView) -> Result<u8, ProgramError> {
    let (addr, bump) = Address::find_program_address(&[AUTHORITY_SEED, wrapped_mint.address().as_ref()], program_id);
    if &addr != authority.address() {
        return Err(WrapError::AuthorityMismatch.into());
    }
    Ok(bump)
}

/// Unwrapped token program must be SPL Token or Token-2022 and own the mint.
fn check_unwrapped_program(program: &AccountView, mint: &AccountView) -> ProgramResult {
    let key = program.address();
    if (key != &TOKEN_PROGRAM && key != &TOKEN_2022_PROGRAM) || !mint.owned_by(key) {
        return Err(WrapError::BadTokenProgram.into());
    }
    Ok(())
}

/// Escrow must be a token account of `program`, for `mint`, owned by `authority`.
/// Returns its current amount.
fn escrow_amount(escrow: &AccountView, program: &AccountView, mint: &AccountView, authority: &AccountView) -> Result<u64, ProgramError> {
    if !escrow.owned_by(program.address()) {
        return Err(WrapError::BadEscrow.into());
    }
    let d = escrow.try_borrow()?;
    if d.len() < ACCOUNT_LEN
        || (d.len() > ACCOUNT_LEN && d[ACCOUNT_LEN] != 2)
        || &d[0..32] != mint.address().as_ref()
        || &d[32..64] != authority.address().as_ref()
    {
        return Err(WrapError::BadEscrow.into());
    }
    Ok(u64::from_le_bytes(d[64..72].try_into().unwrap()))
}

fn mint_decimals(mint: &AccountView) -> Result<u8, ProgramError> {
    let d = mint.try_borrow()?;
    if d.len() < MINT_BASE_LEN || d[45] != 1 {
        return Err(WrapError::BadMint.into());
    }
    Ok(d[44])
}

/// Generic CPI: `metas` are (account, writable, signer), in instruction order.
fn cpi(program: &Address, metas: &[(&AccountView, bool, bool)], data: &[u8], signers: &[Signer]) -> ProgramResult {
    if metas.len() > MAX_CPI_ACCOUNTS {
        return Err(ProgramError::InvalidArgument);
    }
    let mut ix_accounts: [core::mem::MaybeUninit<InstructionAccount>; MAX_CPI_ACCOUNTS] =
        [const { core::mem::MaybeUninit::uninit() }; MAX_CPI_ACCOUNTS];
    let mut views: [core::mem::MaybeUninit<&AccountView>; MAX_CPI_ACCOUNTS] =
        [const { core::mem::MaybeUninit::uninit() }; MAX_CPI_ACCOUNTS];
    for (i, (view, w, s)) in metas.iter().enumerate() {
        ix_accounts[i].write(InstructionAccount::new(view.address(), *w, *s));
        views[i].write(*view);
    }
    let n = metas.len();
    // SAFETY: the first `n` elements were initialized above.
    let (ix_accounts, views) = unsafe {
        (
            core::slice::from_raw_parts(ix_accounts.as_ptr() as *const InstructionAccount, n),
            core::slice::from_raw_parts(views.as_ptr() as *const &AccountView, n),
        )
    };
    let ix = InstructionView { program_id: program, data, accounts: ix_accounts };
    invoke_signed_with_bounds::<MAX_CPI_ACCOUNTS, &AccountView>(&ix, views, signers)
}

/// Token `transfer_checked` with extra (transfer hook) accounts forwarded.
fn transfer_checked(
    program: &AccountView,
    from: &AccountView,
    mint: &AccountView,
    to: &AccountView,
    authority: &AccountView,
    extra: &[AccountView],
    amount: u64,
    decimals: u8,
    signers: &[Signer],
) -> ProgramResult {
    if extra.len() + 4 > MAX_CPI_ACCOUNTS {
        return Err(ProgramError::InvalidArgument);
    }
    let mut metas: [(&AccountView, bool, bool); MAX_CPI_ACCOUNTS] = [(from, false, false); MAX_CPI_ACCOUNTS];
    metas[0] = (from, true, false);
    metas[1] = (mint, false, false);
    metas[2] = (to, true, false);
    metas[3] = (authority, false, true);
    for (i, a) in extra.iter().enumerate() {
        metas[4 + i] = (a, a.is_writable(), a.is_signer());
    }
    let mut data = [0u8; 10];
    data[0] = 12;
    data[1..9].copy_from_slice(&amount.to_le_bytes());
    data[9] = decimals;
    cpi(program.address(), &metas[..4 + extra.len()], &data, signers)
}

fn create_wrapped_mint(program_id: &Address, accounts: &[AccountView]) -> ProgramResult {
    let [payer, wrapped_mint, authority, unwrapped_mint, _system, token_2022, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if token_2022.address() != &TOKEN_2022_PROGRAM {
        return Err(WrapError::BadTokenProgram.into());
    }
    if !unwrapped_mint.owned_by(&TOKEN_PROGRAM) && !unwrapped_mint.owned_by(&TOKEN_2022_PROGRAM) {
        return Err(WrapError::BadTokenProgram.into());
    }
    let mint_bump = [check_wrapped_mint(program_id, wrapped_mint, unwrapped_mint)?];
    let auth_bump = [check_authority(program_id, authority, wrapped_mint)?];
    if !wrapped_mint.is_data_empty() {
        return Err(WrapError::AlreadyCreated.into());
    }

    // Read source mint: decimals, freeze authority, optional in-mint TokenMetadata.
    let decimals = mint_decimals(unwrapped_mint)?;
    let src = unwrapped_mint.try_borrow()?;
    let mut init_mint2 = [0u8; 67];
    init_mint2[0] = 20; // InitializeMint2
    init_mint2[1] = decimals;
    init_mint2[2..34].copy_from_slice(authority.address().as_ref());
    let init_mint2_len = if src[46] == 1 {
        init_mint2[34] = 1;
        init_mint2[35..67].copy_from_slice(&src[50..82]);
        67
    } else {
        35
    };
    // name, symbol, uri as borsh strings: contiguous slice right after update_authority + mint.
    // 8-byte discriminator + borsh(name, symbol, uri).
    const MAX_META: usize = 1024;
    let mut meta_ix = [0u8; MAX_META];
    let meta_len = match find_token_metadata_strings(&src) {
        Some(m) if m.len() <= MAX_META - 8 => {
            meta_ix[..8].copy_from_slice(&TOKEN_METADATA_INIT);
            meta_ix[8..8 + m.len()].copy_from_slice(m);
            m.len()
        }
        Some(_) => return Err(WrapError::BadMint.into()),
        None => 0,
    };
    drop(src);

    let mut final_len = WRAPPED_MINT_BASE_LEN;
    if meta_len > 0 {
        // TLV header 4 + update_authority 32 + mint 32 + strings + empty additional_metadata vec 4
        final_len += 4 + 64 + meta_len + 4;
    }

    let mint_seeds = [
        Seed::from(MINT_SEED),
        Seed::from(unwrapped_mint.address().as_ref()),
        Seed::from(TOKEN_2022_PROGRAM.as_ref()),
        Seed::from(&mint_bump),
    ];
    let mint_signer = [Signer::from(&mint_seeds)];

    // Fund (tolerating lamports pre-sent to the PDA), allocate, assign to Token-2022.
    let needed = Rent::get()?.try_minimum_balance(final_len)?.saturating_sub(wrapped_mint.lamports());
    if needed > 0 {
        let mut d = [0u8; 12];
        d[0] = 2;
        d[4..12].copy_from_slice(&needed.to_le_bytes());
        cpi(&SYSTEM_PROGRAM, &[(payer, true, true), (wrapped_mint, true, false)], &d, &[])?;
    }
    let mut d = [0u8; 12];
    d[0] = 8;
    d[4..12].copy_from_slice(&(WRAPPED_MINT_BASE_LEN as u64).to_le_bytes());
    cpi(&SYSTEM_PROGRAM, &[(wrapped_mint, true, true)], &d, &mint_signer)?;
    let mut d = [0u8; 36];
    d[0] = 1;
    d[4..36].copy_from_slice(TOKEN_2022_PROGRAM.as_ref());
    cpi(&SYSTEM_PROGRAM, &[(wrapped_mint, true, true)], &d, &mint_signer)?;

    // MetadataPointer: authority = mint authority PDA, metadata address = the mint itself.
    let mut d = [0u8; 66];
    d[0] = 39;
    d[1] = 0;
    d[2..34].copy_from_slice(authority.address().as_ref());
    d[34..66].copy_from_slice(wrapped_mint.address().as_ref());
    cpi(&TOKEN_2022_PROGRAM, &[(wrapped_mint, true, false)], &d, &[])?;

    cpi(&TOKEN_2022_PROGRAM, &[(wrapped_mint, true, false)], &init_mint2[..init_mint2_len], &[])?;

    if meta_len > 0 {
        let auth_seeds = [
            Seed::from(AUTHORITY_SEED),
            Seed::from(wrapped_mint.address().as_ref()),
            Seed::from(&auth_bump),
        ];
        cpi(
            &TOKEN_2022_PROGRAM,
            &[
                (wrapped_mint, true, false),
                (authority, false, false),
                (wrapped_mint, false, false),
                (authority, false, true),
            ],
            &meta_ix[..8 + meta_len],
            &[Signer::from(&auth_seeds)],
        )?;
    }
    Ok(())
}

/// Finds in-mint TokenMetadata (Token-2022 TLV) and returns the borsh-encoded
/// `name, symbol, uri` byte range.
fn find_token_metadata_strings(mint: &[u8]) -> Option<&[u8]> {
    if mint.len() <= ACCOUNT_LEN + 1 || mint[ACCOUNT_LEN] != 1 {
        return None;
    }
    let mut p = ACCOUNT_LEN + 1;
    while p + 4 <= mint.len() {
        let ty = u16::from_le_bytes([mint[p], mint[p + 1]]);
        let len = u16::from_le_bytes([mint[p + 2], mint[p + 3]]) as usize;
        let v = mint.get(p + 4..p + 4 + len)?;
        if ty == EXT_TOKEN_METADATA {
            let mut q = 64; // skip update_authority + mint
            for _ in 0..3 {
                let n = u32::from_le_bytes(v.get(q..q + 4)?.try_into().ok()?) as usize;
                q = q.checked_add(4 + n)?;
                if q > v.len() {
                    return None;
                }
            }
            return Some(&v[64..q]);
        }
        if ty == 0 {
            return None;
        }
        p += 4 + len;
    }
    None
}

fn wrap(program_id: &Address, accounts: &[AccountView], amount: u64) -> ProgramResult {
    let [recipient, wrapped_mint, authority, unwrapped_program, token_2022, source, unwrapped_mint, escrow, owner, extra @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if token_2022.address() != &TOKEN_2022_PROGRAM {
        return Err(WrapError::BadTokenProgram.into());
    }
    check_unwrapped_program(unwrapped_program, unwrapped_mint)?;
    check_wrapped_mint(program_id, wrapped_mint, unwrapped_mint)?;
    let bump = [check_authority(program_id, authority, wrapped_mint)?];

    let before = escrow_amount(escrow, unwrapped_program, unwrapped_mint, authority)?;
    let decimals = mint_decimals(unwrapped_mint)?;
    transfer_checked(unwrapped_program, source, unwrapped_mint, escrow, owner, extra, amount, decimals, &[])?;
    let received = escrow_amount(escrow, unwrapped_program, unwrapped_mint, authority)?
        .checked_sub(before)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if received == 0 {
        return Err(WrapError::ZeroAmount.into());
    }

    let seeds = [Seed::from(AUTHORITY_SEED), Seed::from(wrapped_mint.address().as_ref()), Seed::from(&bump)];
    let mut d = [0u8; 9];
    d[0] = 7; // MintTo
    d[1..9].copy_from_slice(&received.to_le_bytes());
    cpi(
        &TOKEN_2022_PROGRAM,
        &[(wrapped_mint, true, false), (recipient, true, false), (authority, false, true)],
        &d,
        &[Signer::from(&seeds)],
    )
}

fn unwrap(program_id: &Address, accounts: &[AccountView], amount: u64) -> ProgramResult {
    let [escrow, recipient, authority, unwrapped_mint, token_2022, unwrapped_program, source, wrapped_mint, owner, extra @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if token_2022.address() != &TOKEN_2022_PROGRAM {
        return Err(WrapError::BadTokenProgram.into());
    }
    check_unwrapped_program(unwrapped_program, unwrapped_mint)?;
    check_wrapped_mint(program_id, wrapped_mint, unwrapped_mint)?;
    let bump = [check_authority(program_id, authority, wrapped_mint)?];
    escrow_amount(escrow, unwrapped_program, unwrapped_mint, authority)?;

    // Burn wrapped tokens; the token program checks `owner` signed.
    let mut d = [0u8; 9];
    d[0] = 8; // Burn
    d[1..9].copy_from_slice(&amount.to_le_bytes());
    cpi(&TOKEN_2022_PROGRAM, &[(source, true, false), (wrapped_mint, true, false), (owner, false, true)], &d, &[])?;

    let decimals = mint_decimals(unwrapped_mint)?;
    let seeds = [Seed::from(AUTHORITY_SEED), Seed::from(wrapped_mint.address().as_ref()), Seed::from(&bump)];
    transfer_checked(
        unwrapped_program,
        escrow,
        unwrapped_mint,
        recipient,
        authority,
        extra,
        amount,
        decimals,
        &[Signer::from(&seeds)],
    )
}
