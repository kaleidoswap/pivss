---
title: "Proof-of-Storage Backups for Lightning and RGB: The PIVSS Prototype"
subtitle: "An incentivized, zero-knowledge backup market — sketched from prompt to working code in a single session."
audience: developer / Bitcoin-native
goal: awareness + trust
variant: technical
date: 2026-07-07
---

# Proof-of-Storage Backups for Lightning and RGB: The PIVSS Prototype

On-chain Bitcoin is forgiving. Lose your device, restore twelve words, recover everything. Lightning and RGB are not. Their state cannot be derived from a seed — and that single fact is one of the quietest, most under-solved risks in self-custody today.

We spent a session turning that problem into working code. The result is **PIVSS** — a *P2P Incentivized Versioned Storage Service*: a backup market where providers get paid in sats to store your encrypted Lightning and RGB state, but only for as long as they can cryptographically **prove** they still hold it. It's an open-source prototype, not a production service. This is what it does, how it works, and why the design matters.

## The problem: state that a seed can't recover

A BIP39 seed deterministically regenerates on-chain keys. It does **not** regenerate:

- **Lightning channel state.** Commitment transactions, revocation secrets, and HTLC state change with every payment. Restore an *old* state and you risk broadcasting a revoked commitment — which hands your channel balance to your counterparty via the penalty mechanism. Static channel backups (SCB) only let you request a cooperative force-close from the peer; in-flight and pending funds are gone.
- **RGB client-side-validated data.** RGB assets live in the client's stash and consignment history. There is no global chain to replay them from. Lose the data, lose the asset. This is the single biggest UX blocker standing between RGB and mainstream custody.

The standard answer is [VSS](https://github.com/lightningdevkit/vss-server) — LDK's Versioned Storage Service, also used by Spark and others. VSS works, but it is a *single trusted provider*: one endpoint, usually run by the wallet vendor as a cost center. If it disappears, gets compromised, or simply decides you're not worth serving, you have no market to fall back on and no cryptographic guarantee it ever held your data honestly.

PIVSS keeps VSS's storage semantics and adds the three things it lacks: a **market**, a **proof**, and **replication**.

## The design

### 1. Providers advertise on nostr

A PIVSS server signs a NIP-01 addressable event (kind `38831`) and publishes it to relays. The event carries everything a client needs to choose a provider: endpoint, price in sats per MiB per billing period, maximum size, supported backup kinds, and a **BOLT12 offer** to pay. In the prototype the default relay is `wss://relay.kaleidoswap.com`; clients discover providers by querying that kind from Rust, or from JS with [NDK](https://github.com/nostr-dev-kit/ndk) or `nostr-tools`. No directory, no gatekeeper.

### 2. Clients upload opaque, already-encrypted blobs

The provider is **zero-knowledge by construction.** PIVSS treats every payload as an opaque blob and assumes it is already encrypted client-side — bring your own ciphertext, e.g. the wallet's VSS `Storable` envelope. The provider, and anyone who compromises it, sees only ciphertext, sizes, and access timing. For clients that don't already encrypt, `pivss-core` ships an optional AES-256-GCM / Argon2id helper, but encryption is a client concern, never the server's.

Re-uploading the same logical backup creates a **new version**. Because VSS keeps only the latest value per key (its "version" is an optimistic-concurrency counter, not history), PIVSS stores each version under its own key — `backups/<id>/v<n>` — which is what makes history retention and per-version distribution possible.

### 3. Every version is seeded over BitTorrent

Each stored version is materialized into a single-file v1 `.torrent` with its own infohash and magnet link, and seeded via [carl](https://github.com/vincenzopalazzo/carl), a privacy-preserving BitTorrent client with nostr-based peer discovery. Data isn't trapped in one provider's database; it becomes a replicable object other peers — and other providers — can pull.

### 4. Proof-of-storage gates the payment

This is the honesty mechanism. Before releasing each recurring payment, the client issues a challenge: a fresh 32-byte nonce plus several random byte ranges. The provider must answer

```
SHA256(nonce ‖ full_bytes)   and   SHA256(nonce ‖ bytes[range_i])
```

read from the versioned store. Because the nonce is fresh every time, the provider **cannot** precompute the answer from a cached digest — it has to read the actual bytes. The client recomputes both against its local copy (or, on the roadmap, a Merkle root) and pays only on a match. Fail the proof, and the money stops. Storage stays honest because revenue is contingent on demonstrable custody, checked continuously.

The reference client automates the whole loop: `pivss-client watch <id>` verifies, then pays, on an interval — and withholds payment the instant a proof fails.

## What actually runs

PIVSS is a three-crate Rust workspace, MIT-licensed, with 21 tests including a full client↔server end-to-end lifecycle over HTTP. In the demo:

- A backup is uploaded; the server stores only the `PIVSS1` encryption envelope — we verified over the wire that the provider holds ciphertext, never plaintext.
- A modified upload becomes **v2**; both versions restore byte-exact, each with its own magnet link.
- A fresh challenge produces a valid proof; a proof minted for one nonce **fails** against a different one; a proof against tampered data **fails** — exactly as the incentive requires.
- The kind `38831` announcement was signed and **published live** to `wss://relay.kaleidoswap.com` and `wss://nos.lol`, both of which accepted it.
- `watch` ran the verify→pay loop and the host panel recorded the earnings.

The VSS backend is pluggable: `memory` for the demo, or point `storage.backend = "vss"` at a real [vss-server](https://github.com/lightningdevkit/vss-server) instance — PIVSS speaks its protobuf API directly, no `protoc` required.

## Why the design matters

Three properties fall out of it:

- **No single-vendor lock-in.** Discovery and pricing are permissionless on nostr; anyone with spare disk and a Lightning node can become a provider. Backup becomes a revenue line, not a cost center.
- **Verifiable custody.** A provider can't take your sats and quietly drop your data. Proof-of-storage makes dishonesty unprofitable in real time.
- **A clear path to trustless availability.** The next step is Reed-Solomon erasure coding across independent providers (any *K* of *N* shards reconstruct), so no single provider going dark can cost you your channels — the torrent layer already gives us the replication primitive.

Two further roadmap items complete the market: **BOLT12 payment proofs** (once [rust-lightning #4297](https://github.com/lightningdevkit/rust-lightning/pull/4297) lands, the payment side becomes cryptographically auditable to match the storage side), and **commitment-based proofs** so a phone can audit a backup it no longer holds locally, using only a 32-byte Merkle root.

## Closing

PIVSS is a prototype, and we're calling it one. But it is a *working, tested, open-source* prototype that takes a real and mostly-ignored problem — Lightning and RGB state you cannot recover from a seed — and shows a concrete, incentive-aligned way out: encrypted by the client, replicated over torrents, discovered on nostr, and paid in sats only against continuous proof that your data is still there.

It also went from idea to running code in a single build session with Claude (Fable) — which is worth noting mostly because it lowers the cost of trying ambitious infrastructure ideas to almost nothing. The interesting part isn't that it was fast; it's that the design is sound enough to build on.

The repository is open under MIT. If you run Lightning or RGB infrastructure — a KaleidoSwap RLN maker node already has the disk, the Lightning node, and working BOLT12 offers — you already have everything you need to be one of the first providers.

📚 **Sources**
- LDK VSS: https://github.com/lightningdevkit/vss-server
- carl (BitTorrent client): https://github.com/vincenzopalazzo/carl
- BOLT12 proof of payment (rust-lightning #4297): https://github.com/lightningdevkit/rust-lightning/pull/4297
- breez-sdk (BOLT12-capable wallet): https://github.com/breez/breez-sdk-liquid
- Demo figures (versioning, proofs, live relay publish) are from this project's own test suite and smoke run.
