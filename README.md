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
| `crates/pivss-server` | axum server: HTTP API, storage backends (`memory`, `vss`), carl seeder, nostr announcements, embedded web UIs |
| `crates/pivss-client` | API client library + CLI (`backup`, `list`, `restore`, `verify`, `pay`, `watch`) |
| `examples/test-backup.json` | the demo backup payload used by the e2e test |

## Quick start (demo mode, no external services)

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
- **BOLT12** — set `bolt12_offer` to an offer from your node (RLN `/lnoffer`,
  CLN `offer`, LDK-node). Clients with a real wallet
  (e.g. [breez-sdk](https://github.com/breez/breez-sdk-liquid), which can pay
  offers) pay that instead of the mock endpoint.
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
| `POST /api/v1/backups/{id}/payments` | record payment (mock today) |
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
- **BOLT12 payment proofs** — once
  [rust-lightning #4297](https://github.com/lightningdevkit/rust-lightning/pull/4297)
  ships, replace the mock payment endpoint with cryptographic
  proof-of-payment tied to the offer; the server can then publish payment
  receipts alongside storage proofs (fully auditable market).
- **Real wallet in the client** — embed breez-sdk behind a `Payer` trait so
  `watch` pays the announced offer directly.
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
versions → proofs incl. failure cases → payment accounting).
