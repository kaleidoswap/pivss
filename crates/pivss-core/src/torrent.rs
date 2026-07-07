//! Minimal single-file BitTorrent v1 metainfo (.torrent) creation.
//!
//! Enough to hand a .torrent + data file to an external seeder such as
//! `carl seed <file.torrent> <data-dir>` and to advertise a magnet link.

use sha1::{Digest, Sha1};

const PIECE_LENGTH: usize = 256 * 1024;

/// Bencode encoder for the small subset we need (int, bytes, dict, list).
#[derive(Default)]
pub struct Bencoder {
    out: Vec<u8>,
}

impl Bencoder {
    pub fn int(&mut self, v: i64) {
        self.out.extend_from_slice(format!("i{v}e").as_bytes());
    }

    pub fn bytes(&mut self, v: &[u8]) {
        self.out
            .extend_from_slice(format!("{}:", v.len()).as_bytes());
        self.out.extend_from_slice(v);
    }

    pub fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }

    /// Keys must be inserted in lexicographic order by the caller.
    pub fn dict(&mut self, f: impl FnOnce(&mut Self)) {
        self.out.push(b'd');
        f(self);
        self.out.push(b'e');
    }

    pub fn finish(self) -> Vec<u8> {
        self.out
    }
}

pub struct TorrentInfo {
    /// Raw .torrent file bytes.
    pub metainfo: Vec<u8>,
    /// v1 infohash (SHA1 of the bencoded info dict), hex-encoded.
    pub infohash: String,
    /// magnet:?xt=urn:btih:... link.
    pub magnet: String,
}

/// Build a single-file v1 torrent for `data` named `name`.
///
/// `trackers` may be empty (DHT / nostr-discovery only, as supported by carl).
pub fn create_torrent(name: &str, data: &[u8], trackers: &[String]) -> TorrentInfo {
    // Piece hashes: SHA1 of each fixed-size piece, concatenated.
    let mut pieces = Vec::new();
    for chunk in data.chunks(PIECE_LENGTH).filter(|c| !c.is_empty()) {
        pieces.extend_from_slice(&Sha1::digest(chunk));
    }
    if data.is_empty() {
        pieces.extend_from_slice(&Sha1::digest(data));
    }

    // info dict — keys in lexicographic order: length, name, piece length, pieces
    let mut info = Bencoder::default();
    info.dict(|b| {
        b.str("length");
        b.int(data.len() as i64);
        b.str("name");
        b.str(name);
        b.str("piece length");
        b.int(PIECE_LENGTH as i64);
        b.str("pieces");
        b.bytes(&pieces);
    });
    let info_bytes = info.finish();
    let infohash_raw = Sha1::digest(&info_bytes);
    let infohash = hex::encode(infohash_raw);

    // top-level dict — keys in order: announce, announce-list?, created by, info
    let mut top = Bencoder::default();
    top.out.push(b'd');
    if let Some(first) = trackers.first() {
        let mut b = Bencoder::default();
        b.str("announce");
        b.str(first);
        b.str("announce-list");
        b.out.push(b'l');
        for t in trackers {
            b.out.push(b'l');
            b.str(t);
            b.out.push(b'e');
        }
        b.out.push(b'e');
        top.out.extend_from_slice(&b.finish());
    }
    {
        let mut b = Bencoder::default();
        b.str("created by");
        b.str("pivss");
        b.str("info");
        top.out.extend_from_slice(&b.finish());
        top.out.extend_from_slice(&info_bytes);
    }
    top.out.push(b'e');

    let mut magnet = format!("magnet:?xt=urn:btih:{}&dn={}", infohash, urlencode(name));
    for t in trackers {
        magnet.push_str("&tr=");
        magnet.push_str(&urlencode(t));
    }

    TorrentInfo {
        metainfo: top.finish(),
        infohash,
        magnet,
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torrent_is_deterministic_and_wellformed() {
        let data = b"pivss test backup payload".repeat(1000);
        let t1 = create_torrent("backup.json", &data, &[]);
        let t2 = create_torrent("backup.json", &data, &[]);
        assert_eq!(t1.infohash, t2.infohash);
        assert_eq!(t1.infohash.len(), 40);
        assert!(t1.magnet.starts_with("magnet:?xt=urn:btih:"));
        // bencode sanity: dict wrapper and an info dict with our name inside
        assert_eq!(t1.metainfo.first(), Some(&b'd'));
        assert_eq!(t1.metainfo.last(), Some(&b'e'));
        let hay = String::from_utf8_lossy(&t1.metainfo).into_owned();
        assert!(hay.contains("4:infod"));
        assert!(hay.contains("11:backup.json"));
    }

    #[test]
    fn tracker_lands_in_metainfo_and_magnet() {
        let t = create_torrent(
            "f.bin",
            b"x",
            &["udp://tracker.example.com:6969".to_string()],
        );
        let hay = String::from_utf8_lossy(&t.metainfo).into_owned();
        assert!(hay.contains("8:announce30:udp://tracker.example.com:6969"));
        assert!(t
            .magnet
            .contains("&tr=udp%3A%2F%2Ftracker.example.com%3A6969"));
    }

    #[test]
    fn multi_piece_hashes() {
        let data = vec![7u8; PIECE_LENGTH * 2 + 100]; // 3 pieces
        let t = create_torrent("big.bin", &data, &[]);
        let hay = t.metainfo;
        // "pieces" value is 3 * 20 bytes => length prefix "60:"
        let needle = b"6:pieces60:";
        assert!(hay.windows(needle.len()).any(|w| w == needle.as_slice()));
    }
}
