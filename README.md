# darkpool-wrap

Minimal 1:1 token wrapper for Solana, written with [pinocchio](https://github.com/anza-xyz/pinocchio).
It wraps any SPL Token or Token-2022 mint (including mints with extensions such as
`PausableConfig`, `DefaultAccountState`, `PermanentDelegate`, `TransferHook`,
`ScaledUiAmount`) into a plain Token-2022 mint that carries only
`MetadataPointer` and `TokenMetadata`.

The logic and PDA scheme follow [SPL token-wrap](https://github.com/solana-program/token-wrap),
reduced to three instructions.

Program ID (mainnet): `4WMGnz33sRKHynF2an3kWmfZtd8eudTVCExtSmMWuaCc`

## Instructions

| # | Name | Data |
|---|------|------|
| 0 | `CreateWrappedMint` | `[0]` |
| 1 | `Wrap` | `[1, amount: u64 LE]` |
| 2 | `Unwrap` | `[2, amount: u64 LE]` |

Account lists are documented at the top of [`src/lib.rs`](src/lib.rs). `Wrap` and
`Unwrap` use the same account order as SPL token-wrap.

## Addresses

- Wrapped mint: PDA `["mint", unwrapped_mint, token_2022_program_id]`
- Mint authority: PDA `["authority", wrapped_mint]`
- Escrow: any token account of the unwrapped mint owned by the mint authority
  PDA (clients use its associated token account)

## Guarantees

- **1:1 backing.** `Wrap` mints exactly the amount the escrow received, so a
  transfer fee on the unwrapped mint can't create unbacked wrapped tokens.
  `Unwrap` burns first, then releases the same amount from escrow.
- **No admin.** The program holds no keys and has no instruction that changes a
  wrapped mint after creation.
- **Copied, not supplied.** Decimals, freeze authority and (if the source mint has
  in-mint `TokenMetadata`) name, symbol and URI are read on-chain from the
  unwrapped mint. Callers can't attach arbitrary metadata to a wrapped mint.
  The metadata update authority is the mint authority PDA, which has no update
  path, so the metadata is immutable.
- **Transfer hooks.** Extra accounts after the fixed list are forwarded to
  `transfer_checked`.

## Not included

Multisig signers, metadata sync, Metaplex metadata, and closing stuck escrows.

## Trust assumptions inherited from the unwrapped mint

The wrapper can't remove powers the unwrapped mint's authorities already have.
A permanent delegate can move escrowed tokens, a pause authority can block
wrap/unwrap, and the freeze authority is copied to the wrapped mint.

## Build

```
cargo build-sbf
```

Verifiable build (the crate has no `solana-program` dependency, so the image is pinned explicitly):

```
solana-verify build --base-image solanafoundation/solana-verifiable-build:4.3.0
solana-verify get-executable-hash target/deploy/darkpool_wrap.so
# ac59d863472d93dd7e251f680d6417b3d21018dedef69971651181a6c982a8bc

solana-verify verify-from-repo --remote \
  --program-id 4WMGnz33sRKHynF2an3kWmfZtd8eudTVCExtSmMWuaCc \
  --base-image solanafoundation/solana-verifiable-build:4.3.0 \
  --library-name darkpool_wrap \
  https://github.com/sipLinchik/darkpool-wrap
```

## License

Apache-2.0
