# PIVSS — P2P Incentivized Versioned Storage Service

An incentivized backup system for **Lightning** and **RGB** wallet state:
servers store versioned backups (VSS semantics), seed them over **BitTorrent**,
advertise themselves on **nostr** with a price in sats and a **BOLT12 offer**,
and get paid recurringly — but only while they can *prove* they still hold the
data.

```
┌──────────┐  upload backup (versioned)   ┌──────────────┐   putObject     ┌────────────┐
│  client   │ ───────────────────────────▶ │ pivss-server │ ──────────────▶ │ VSS store  │
│ (wallet)  │                              │              │                 │ (LDK vss-  │
│           │  ◀─────────────────────────  │              │ ◀────────────── │  server /  │
│ breez-sdk │   magnet link + quote        │              │    getObject    │  memory)   │
│ (BOLT12)  │                              └──────┬───────┘                 └────────────┘
│           │  challenge(nonce, ranges)           │ seeds every version
│           │ ───────────────────────────▶        ▼
│           │  ◀─ proof = SHA256(nonce‖bytes) ┌────────┐      ┌─────────────┐
│           │                                 │  carl  │      │ nostr relays│
│           │  proof OK → pay BOLT12 offer    │ (seed) │      │ kind 38831  │
│           │ ─────────────────────────────▶  └────────┘      │ price+offer │
└──────────┘                                       ▲          └─────────────┘
                                                   └── announce ──────┘
```

## The incentive loop

1. **Server advertises** on nostr (addressable event, kind `38831`): endpoint,
   price in sats/MiB per billing period, max size, and its BOLT12 offer.
2. **Client uploads** a backup — an **opaque, already-encrypted blob** (the
   example is [examples/test-backup.json](examples/test-backup.json), a fake
   LN+RGB state snapshot). Re-uploading the same file name creates a **new
   version**; history is retained and every version gets its own torrent +
   magnet link.
3. **Client periodically challenges** the server: a fresh 32-byte nonce plus
   random byte ranges. The server must answer `SHA256(nonce ‖ bytes)` read
   from the versioned store — it can't precompute this from a cached digest,
   so a valid proof means the data is really still there.
4. **Proof OK → client pays** the BOLT12 offer for another billing period
   (`pivss-client watch` automates verify→pay). Proof fails → payment is
   withheld. Storage stays honest because revenue stops the moment data drops.
   Payment is a **real** BOLT12 payment via an embedded
   [breez-sdk-liquid](https://github.com/breez/breez-sdk-liquid) wallet on
   both sides once `lightning.enable = true` (`--real-payment` on the
   client) — the provider only ever records a payment its own wallet
   observed, never a client's claim. See [Real integrations](#real-integrations).

## Trust model

**The provider is zero-knowledge.** PIVSS treats every payload as an opaque
blob and assumes it is *already encrypted* client-side (bring your own
ciphertext — e.g. the wallet's VSS `Storable` envelope). The provider, and
anyone who compromises it, sees only ciphertext, sizes, and access timing —
never your channel state or RGB assets. For clients that don't already
encrypt, `pivss-core::crypto` is an optional AES-256-GCM / Argon2id helper
(the reference CLI wires it to `--passphrase`), but encryption is a client
concern by design, not the server's.

Availability is *not* yet trustless with a single provider — that's what the
multi-provider erasure-coding roadmap item below addresses. Today a provider
can still go offline; it just can't lie about holding your data (proof-of-
storage) or read it (encryption).

## Workspace layout

| crate / dir | what it is |
|---|---|
| `crates/pivss-core` | shared primitives: VSS protobuf types (hand-written, `protoc`-free), optional AES-256-GCM/Argon2id encryption envelope, single-file v1 torrent creation + magnet links, proof-of-storage challenges, minimal NIP-01 nostr signing |
| `crates/pivss-ln` | thin wrapper around `breez-sdk-liquid`: durable BOLT12 offer creation, paying an offer with a payer note, forwarding confirmed incoming payments |
| `crates/pivss-server` | axum server: HTTP API, storage backends (`memory`, `vss`), carl seeder, nostr announcements, real/mock payments, embedded web UIs |
| `crates/pivss-client` | API client library + CLI (`backup`, `list`, `restore`, `verify`, `pay`, `watch`, `discover`) |
| `examples/test-backup.json` | the demo backup payload used by the e2e test |

## Quick start — Docker (recommended, no Rust toolchain needed)

```bash
docker compose up -d server
```

That's it — demo mode (memory storage, mock payments), no config, no API key.
Open **http://localhost:8339/app** in a browser: it's a complete zero-install
web client — pick a file, upload it, verify proof-of-storage, pay, and
**discover other PIVSS providers via nostr** — all from the browser, no CLI
required. The **http://localhost:8339/panel** host view shows what the
server sees (offer, backups, seeding, earnings) and publishes its own
announcement.

Prefer the CLI, or want to script `watch`? It's dockerized too, no local Rust
needed — put files to upload in `./uploads/`:

```bash
docker compose run --rm client backup /uploads/mybackup.json --kind lightning
docker compose run --rm client list
docker compose run --rm client verify <backup_id>
docker compose run --rm client watch <backup_id> --interval 60
```

For real BOLT12 payments instead of mock, put your
[Breez API key](https://breez.technology) in `.env` (`cp .env.example .env`),
set `[lightning] enable = true` + `network = "mainnet"` in
`config.docker.toml`, then `docker compose up -d server` again.

## Quick start — Cargo (for development)

```bash
cargo run -p pivss-server
# host panel:  http://127.0.0.1:8339/panel
# client app:  http://127.0.0.1:8339/app
```

In another shell:

```bash
# upload the example test file as a lightning backup
cargo run -p pivss-client -- backup examples/test-backup.json --kind lightning --label "demo node"

# edit the file, upload again → version 2 (same backup id)
cargo run -p pivss-client -- list

# challenge the server to prove storage, pay only if it does
cargo run -p pivss-client -- verify <backup_id>
cargo run -p pivss-client -- pay <backup_id>

# or automate the whole incentive loop (verify → pay, every 60s)
cargo run -p pivss-client -- watch <backup_id> --interval 60

# restore any version
cargo run -p pivss-client -- restore <backup_id> --version 1 -o restored.json

# real payments instead of mock (needs breez-sdk-liquid regtest stack or a
# mainnet Breez API key — see "Real integrations" below)
cargo run -p pivss-client -- pay <backup_id> --real-payment --ln-network mainnet

# find providers by querying nostr for their announcements (signature-verified)
cargo run -p pivss-client -- discover
```

## Real integrations

- **VSS backend** — run [vss-server](https://github.com/lightningdevkit/vss-server)
  and set `storage.backend = "vss"` + `storage.vss_url` in `config.toml`.
  PIVSS speaks its protobuf API (`putObject`/`getObject`/`listKeyVersions`).
  Note VSS keeps only the latest value per key (its version is an
  optimistic-concurrency counter), so PIVSS stores each backup version under
  its own key — that's what makes history retention and per-version torrents
  possible.
- **Torrent seeding** — install [carl](https://github.com/vincenzopalazzo/carl)
  and the server spawns `carl seed <torrent> <dir> --port N` per version.
  Without carl, `.torrent` files + payload are still materialized under
  `<data_dir>/seeds/` (magnet links included), so any client can seed them —
  carl's nostr-based peer discovery pairs nicely with the announcement.
- **BOLT12 (real payments)** — set `[lightning] enable = true` in
  `config.toml`. The server connects a
  [breez-sdk-liquid](https://github.com/breez/breez-sdk-liquid) wallet,
  creates a durable BOLT12 offer (replacing the static `bolt12_offer`
  string in the announcement), and a background task drains its payment
  event stream: a confirmed incoming payment is correlated to a backup by
  its **payer note** (the client sets this to the `backup_id`) and only then
  written as a `PaymentRecord` — the mock endpoint
  (`POST /backups/{id}/payments`) is disabled while a real wallet is
  connected, so a client can never simply assert it paid. `network` is
  `"regtest"` (no API key, but needs a local Breez regtest stack — see
  [breez-sdk-liquid/regtest](https://github.com/breez/breez-sdk-liquid/tree/main/regtest))
  or `"mainnet"` (needs a free [Breez API key](https://breez.technology));
  `"testnet"` is not supported by the underlying SDK. The client pays the
  same way: `pivss-client pay <id> --real-payment` (or `--network mainnet
  --ln-api-key ...`) embeds its own separate breez-sdk-liquid wallet,
  generating and persisting its own mnemonic under `--state-dir`. Without
  `--real-payment`, the client keeps using the mock endpoint — this is the
  default and needs no wallet, API key, or regtest stack, which is what
  keeps the zero-setup demo working.
- **Nostr** — the server signs a NIP-01 addressable event (kind `38831`) and
  publishes it to the configured relays (default `wss://relay.kaleidoswap.com`
  + `wss://nos.lol`). Clients discover providers and their prices/offers by
  querying that kind on relays — from Rust, or from JS with
  [NDK](https://github.com/nostr-dev-kit/ndk) / `nostr-tools`.

## API

| method & path | purpose |
|---|---|
| `GET /api/v1/info` | service announcement, nostr identity, stats |
| `POST /api/v1/backups?filename=&kind=&label=` (raw body) | store a new backup version |
| `GET /api/v1/backups` / `GET /api/v1/backups/{id}` | list / inspect manifests |
| `GET /api/v1/backups/{id}/versions/{v}/data` | restore bytes |
| `POST /api/v1/backups/{id}/challenge` | proof-of-storage challenge |
| `POST /api/v1/backups/{id}/payments` | record a client-asserted payment — **403 whenever a real wallet is connected** (`lightning.enable = true`); demo/CI only |
| `GET /api/v1/payments` | payments received |
| `POST /api/v1/announce` (`{"dry_run":true}` to preview) | sign + publish the nostr announcement |

## Roadmap

- **Multi-provider erasure coding** — split the encrypted backup into N
  Reed-Solomon shards (any K reconstruct) across independent providers
  discovered on nostr. This is the leap from "better VSS" to *availability*
  that survives any single provider vanishing.
- **Commitment-based proofs** — commit a Merkle root at upload so a thin
  client (a phone) can audit a backup it no longer holds locally, using only
  the 32-byte root instead of the full file.
- **L402 upload gating** — require a paid L402 macaroon to `POST /backups`,
  so storage is prepaid from the first byte.
- **Non-repudiable payment proofs** — payments today are real (the
  provider's own wallet observes them), but that's the provider's word to
  the client. Once
  [rust-lightning #4297](https://github.com/lightningdevkit/rust-lightning/pull/4297)
  ships, add cryptographic proof-of-payment tied to the offer so a *third
  party* — not just the provider — can verify a payment happened, making
  the whole market auditable.
- **Stronger payment correlation** — payer-note matching is enough for a
  small pilot but isn't hardened against a malicious relay/MITM; consider a
  per-request expected-amount/nonce check before treating a note as
  authoritative.
- **Reputation via nostr** — providers accumulate uptime + proof-success
  history under their identity so the market can self-police.

## Tests

```bash
cargo test --workspace
```

21 tests cover: client-side encryption (roundtrip / wrong-passphrase /
deterministic reproduction / fresh salt+nonce / envelope detection), torrent
determinism + bencode correctness, proof-of-storage (honest / tampered /
replayed-nonce / empty file), nostr sign+verify + announcement events, VSS
versioning semantics (conflicts, `-1` writes), protobuf roundtrips, and a full
client↔server e2e lifecycle over HTTP (upload → version bump → restore both
versions → proofs incl. failure cases → payment accounting). This e2e suite
runs entirely with `lightning.enable = false` (the default) — no network
dependency, no API key, no regtest stack.

`pivss-ln` (the breez-sdk-liquid wrapper) has no automated tests: paying and
receiving only mean something against a live wallet, which needs either a
Breez API key + mainnet, or a local Breez regtest stack. It's verified by
type-checking against the pinned SDK version, a clean workspace build, and a
manual run confirming the server fails fast with a clear error when
`lightning.enable = true` and no regtest stack is reachable — not yet by an
end-to-end real payment.
