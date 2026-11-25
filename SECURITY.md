# Security Policy

`darkpool-wrap` holds user funds in escrow on Solana mainnet. We take reports seriously.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

- Preferred: [report privately on GitHub](https://github.com/sipLinchik/darkpool-wrap/security/advisories/new)
  (Security → Report a vulnerability).
- Alternative: DM [@darkpoolrings](https://x.com/darkpoolrings) on X and we will open a private channel.

Please include the affected instruction, a description of the impact, and if possible a transaction or a
test (see `tests/wrap.test.mjs`) that reproduces it. We aim to acknowledge reports within 72 hours.

## Scope

| In scope | Out of scope |
|---|---|
| Program `4WMGnz33sRKHynF2an3kWmfZtd8eudTVCExtSmMWuaCc` on mainnet and the code in this repository | Powers of the source mint's issuer (freeze, permanent delegate, pause, transfer hook). They are documented in the README and are by design |
| Breaking 1:1 backing, unauthorized mint/unwrap, escrow drain, PDA squatting, metadata spoofing | Front-ends, RPC providers, Helius Privacy Rings itself |

## Status

The program is **unaudited**. The upgrade authority is listed in the [README](README.md#deployment); check it on-chain
before relying on it. The deployed binary can be checked against this repository with `solana-verify`
(see [Reproducible build](README.md#reproducible-build)).
