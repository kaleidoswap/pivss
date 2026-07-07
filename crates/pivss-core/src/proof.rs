//! Proof-of-storage: nonce-based challenge/response.
//!
//! The client keeps a local copy (or at minimum the SHA256) of every backup
//! version it uploaded. To check the server still holds the data before
//! releasing a recurring BOLT12 payment, it sends a random nonce plus a few
//! random byte ranges. The server must answer with:
//!
//! - `full_proof`  = SHA256(nonce || full_file_bytes)
//! - `range_proofs[i]` = SHA256(nonce || file[range_i])
//!
//! Because the nonce is fresh, the server cannot precompute answers from a
//! stored digest — it must read the actual bytes. The client recomputes both
//! from its local copy and pays only on a match.

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ByteRange {
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageChallenge {
    /// 32 random bytes, hex-encoded.
    pub nonce: String,
    pub ranges: Vec<ByteRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageProof {
    pub nonce: String,
    /// SHA256(nonce_bytes || file_bytes), hex.
    pub full_proof: String,
    /// SHA256(nonce_bytes || file[range]) per requested range, hex.
    pub range_proofs: Vec<String>,
    /// Plain SHA256 of the file, hex — lets thin clients that only kept the
    /// digest at upload time at least check content identity.
    pub file_sha256: String,
}

/// Create a challenge with `n_ranges` random ranges over a file of `file_len` bytes.
pub fn make_challenge(file_len: u64, n_ranges: usize) -> StorageChallenge {
    let mut rng = rand::thread_rng();
    let mut nonce = [0u8; 32];
    rng.fill_bytes(&mut nonce);

    let mut ranges = Vec::new();
    if file_len > 0 {
        for _ in 0..n_ranges {
            // file_len >= 1 here (guarded above), and 1 <= 4096, so clamp is safe.
            let max_len = file_len.clamp(1, 4096);
            let length = 1 + (rng.next_u64() % max_len);
            let offset = rng.next_u64() % (file_len.saturating_sub(length) + 1);
            ranges.push(ByteRange { offset, length });
        }
    }
    StorageChallenge {
        nonce: hex::encode(nonce),
        ranges,
    }
}

/// Compute the proof for `data` — used by the server to respond, and by the
/// client (against its local copy) to verify.
pub fn compute_proof(challenge: &StorageChallenge, data: &[u8]) -> StorageProof {
    let nonce = hex::decode(&challenge.nonce).unwrap_or_default();

    let mut h = Sha256::new();
    h.update(&nonce);
    h.update(data);
    let full_proof = hex::encode(h.finalize());

    let range_proofs = challenge
        .ranges
        .iter()
        .map(|r| {
            let start = (r.offset as usize).min(data.len());
            let end = (r.offset + r.length).min(data.len() as u64) as usize;
            let mut h = Sha256::new();
            h.update(&nonce);
            h.update(&data[start..end]);
            hex::encode(h.finalize())
        })
        .collect();

    StorageProof {
        nonce: challenge.nonce.clone(),
        full_proof,
        range_proofs,
        file_sha256: hex::encode(Sha256::digest(data)),
    }
}

/// Client-side check: recompute against the local copy and compare.
pub fn verify_proof(challenge: &StorageChallenge, local_data: &[u8], proof: &StorageProof) -> bool {
    if proof.nonce != challenge.nonce {
        return false;
    }
    let expected = compute_proof(challenge, local_data);
    expected.full_proof == proof.full_proof && expected.range_proofs == proof.range_proofs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honest_server_passes() {
        let data = b"lightning channel state backup v42".repeat(500);
        let ch = make_challenge(data.len() as u64, 3);
        let proof = compute_proof(&ch, &data);
        assert!(verify_proof(&ch, &data, &proof));
    }

    #[test]
    fn tampered_data_fails() {
        let data = b"original".repeat(1000).to_vec();
        let ch = make_challenge(data.len() as u64, 3);
        let mut tampered = data.clone();
        tampered[100] ^= 0xff;
        let proof = compute_proof(&ch, &tampered);
        assert!(!verify_proof(&ch, &data, &proof));
    }

    #[test]
    fn replayed_nonce_fails() {
        let data = b"data".repeat(100).to_vec();
        let ch1 = make_challenge(data.len() as u64, 2);
        let ch2 = StorageChallenge {
            nonce: hex::encode([9u8; 32]),
            ranges: ch1.ranges.clone(),
        };
        let stale = compute_proof(&ch2, &data);
        assert!(!verify_proof(&ch1, &data, &stale));
    }

    #[test]
    fn empty_file_roundtrip() {
        let ch = make_challenge(0, 3);
        assert!(ch.ranges.is_empty());
        let proof = compute_proof(&ch, b"");
        assert!(verify_proof(&ch, b"", &proof));
    }
}
