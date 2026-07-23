# PIVSS vs. Storm — a design comparison

_Draft / design note. Not committed as doctrine; a starting point for discussion._

Both PIVSS and [Storm](https://github.com/Storm-WG/storm-spec) answer the same
question: **how do you pay someone to store your data without trusting them to
actually keep it?** They reach very different answers, and the difference is
instructive because Storm is, in large part, where PIVSS's own roadmap points.

Two things to hold in mind while reading:

- **Storm is a specification**, authored in the LNP/BP working group (the same
  ecosystem lineage as RGB), and effectively dormant since ~2019–2020. There is
  no shipped, mainnet implementation in the spec repo.
- **PIVSS is a running MVP** — Rust, real BOLT12 payments on Bitcoin mainnet,
  `docker compose up` to try.

So this is as much *paper vs. product* as it is *design vs. design*.

---

## Side by side

| Dimension | Storm | PIVSS |
|---|---|---|
| **Status** | Spec only, dormant | Runs on mainnet today |
| **Provider skin-in-the-game** | Stakes collateral in a funding tx; forfeits it on data loss | None — a dishonest provider only forgoes *future* payments |
| **Payment ↔ proof binding** | Atomic: HTLC + CSV settlement on a shared funding contract; the provider claims funds only by revealing the decryption key | Advisory: the client verifies a proof, then *chooses* whether to pay the BOLT12 offer. Nothing on-chain couples the two |
| **Proof mechanism** | Merkle trees over encrypted + plaintext chunks (256 B – 1 KB); a random oracle samples 1–10% of chunks; chunk pairs + Merkle paths returned | Fresh 32-byte nonce + random byte ranges; provider returns `SHA256(nonce ‖ file)` and `SHA256(nonce ‖ file[range])`. No tree, no sampling |
| **Does the verifier need the file?** | No — thin verifier checks sampled leaves via Merkle paths | **Yes** — the verifier (wallet) holds the plaintext |
| **Anti-outsourcing** | Per-contract encryption keypair binds the ciphertext to the proof (a proof-of-replication direction) | Not addressed (explicit roadmap item) |
| **Payment rail** | Raw HTLC / CSV outputs, proposed into LN commitment transactions | BOLT12 offers via breez-sdk-liquid |
| **Discovery** | None in the spec | nostr marketplace, kind `38831` |
| **Versioning** | No | Yes — full version history (VSS) |
| **Redundancy** | Partial replication across providers (BitTorrent-like), sketched | Full copies fan-out today; erasure coding on roadmap |
| **Scope** | Storage **and** incentivized messaging | Storage only |
| **Layer** | A protocol: needs custom Bitcoin script + channel construction | An application on existing infra: no consensus or channel changes |

---

## The honest read

**Storm is the more rigorous protocol.** Its bilateral bonding — the provider
stakes collateral, and payment is atomic with the reveal of the decryption key —
makes it genuinely trust-minimised and griefing-resistant in a way PIVSS is not.
Its Merkle-chunk-plus-sampling proof is precisely the "thin client" and
"anti-outsourcing" design that PIVSS currently lists as *future work*. Put
bluntly: **much of PIVSS's roadmap is "become more like Storm."**

**PIVSS's advantage is that it exists.** Storm requires custom on-chain funding
contracts and deep Lightning integration that were never shipped. PIVSS trades
that sophistication for something you can run today, built on modern BOLT12 +
nostr + plain HTTP, with real payments on mainnet and zero setup. It is a
product; Storm is a paper.

The two are not really competitors so much as two points on the same curve: one
optimised for protocol-level guarantees, the other for shipping now.

---

## What's worth borrowing from Storm

Ordered by trust-per-unit-effort:

1. **Provider stake bonded in a contract.** Today a dishonest PIVSS provider's
   only penalty is lost future income. A forfeitable stake gives real
   skin-in-the-game for data loss. This is the single largest trust upgrade
   available.
2. **Atomic payment ↔ data/key reveal.** Storm settles so the provider can only
   take the money by revealing the key. That closes the "client verifies the
   proof, then decides whether to pay" gap that PIVSS has by construction.
3. **Merkle chunk commitments + random sampling.** Enables thin-client proofs
   for wallets that keep only a commitment rather than the whole file — already
   a PIVSS roadmap item, and Storm is a good reference design for it.
4. **Per-contract encryption keypair bound into the proof.** A concrete path
   toward proof-of-replication / anti-outsourcing — PIVSS's other open problem.

### The catch

Adopting (1) and (2) turns PIVSS from "an app that uses BOLT12" into "a protocol
that needs custom contract scripts" — which is exactly the complexity that left
Storm on the shelf for half a decade. The interesting question is not *whether*
to copy Storm but whether we can get its stake + atomicity guarantees with
something **lighter** than its full funding-transaction machinery:

- **Hold invoices / PTLCs** over Lightning to bind payment to a revealed secret,
  instead of a bespoke on-chain funding contract.
- A **stake held in the provider's own Lightning/Liquid balance** with a
  challenge-driven slashing path, rather than a two-party funding output.
- Keeping the proof off-chain (as today) but making **settlement** the atomic
  step, so the cryptographic proof stays cheap while the *economic* guarantee
  gets teeth.

That middle ground — Storm's guarantees, PIVSS's deployability — is the design
space worth exploring next.

---

## What PIVSS has that Storm doesn't

Not everything flows one way:

- **A discovery marketplace.** nostr kind-`38831` announcements let clients find
  and compare providers. Storm has no discovery layer.
- **Versioning.** PIVSS keeps every version of a backup (VSS semantics), which
  matters for the target use case: rolling Lightning channel state and RGB
  stashes.
- **Modern, minimal rails.** BOLT12 + nostr + HTTP, no channel or consensus
  changes required to run a provider.
- **It runs.** The whole loop — advertise, upload, challenge, prove, pay — works
  end-to-end on mainnet today.
