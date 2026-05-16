//! Supplementary tracker list shipped with kino. Locked by PRD §8.
//!
//! The list is appended to the `announce`/`announce-list` entries of every
//! added torrent or magnet. Changes require a human PRD revision.

/// PRD §8 supplementary trackers, in spec order.
pub const SUPPLEMENTARY_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://open.tracker.cl:1337/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://opentracker.i2p.rocks:6969/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://explodie.org:6969/announce",
    "udp://tracker.moeking.me:6969/announce",
    "udp://tracker.bittor.pw:1337/announce",
    "udp://retracker.lanta-net.ru:2710/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://tracker.tiny-vps.com:6969/announce",
    "udp://www.torrent.eu.org:451/announce",
    "udp://tracker.dler.org:6969/announce",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_matches_prd() {
        // PRD §8 enumerates 14 trackers.
        assert_eq!(SUPPLEMENTARY_TRACKERS.len(), 14);
    }

    #[test]
    fn all_are_udp_announce_urls() {
        for url in SUPPLEMENTARY_TRACKERS {
            assert!(url.starts_with("udp://"), "{url} is not a udp:// URL");
            assert!(
                url.ends_with("/announce"),
                "{url} does not end with /announce"
            );
        }
    }

    #[test]
    fn no_duplicates() {
        let mut sorted = SUPPLEMENTARY_TRACKERS.to_vec();
        sorted.sort_unstable();
        let original_len = sorted.len();
        sorted.dedup();
        assert_eq!(sorted.len(), original_len);
    }
}
