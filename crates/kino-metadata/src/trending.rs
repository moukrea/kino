//! Trending aggregation with diversity (PRD §F-004).
//!
//! Pure logic: takes per-provider [`ProviderItem`] lists plus the daily seed
//! inputs, returns the final ordered [`TitleSummary`] list rendered on the
//! Home screen. Lives in `kino-metadata` because every input comes from the
//! metadata clients and the seeded-shuffle PRNG is a metadata concern; the
//! aggregator itself never touches the database or the network.
//!
//! ## Locked algorithm (PRD §F-004)
//!
//! 1. Fetch up to 100 trending items from each enabled provider (TMDB,
//!    Trakt, TVDB) — this happens at the call site.
//! 2. Deduplicate by item id (`IMDb` when available; provider-prefixed
//!    fallback otherwise — see ADR-049).
//! 3. Normalize per-provider rank to `[0..1]` where 0 is best.
//! 4. Weighted score: `0.45 * trakt + 0.35 * tmdb + 0.20 * tvdb`. Missing
//!    ranks default to `0.5` (neutral).
//! 5. Split into Top-Trending (top quartile by score) and Hidden-Gems
//!    (`rating` > [`HIDDEN_GEMS_RATING_THRESHOLD`] AND `popularity_rank`
//!    < median(`popularity_rank`) of the fetched set), with Hidden-Gems
//!    excluding anything already in Top-Trending.
//! 6. Alternate the two pools per [`FINAL_LIST_PATTERN`] (`[T,T,T,G,G]`
//!    repeating) until [`TRENDING_RESULT_COUNT`] items are picked. If a
//!    pool runs out, fill from the other.
//! 7. Shuffle the final list with `ChaCha20Rng::from_seed(SHA256(date ||
//!    install_id))` so two same-day invocations are identical and two
//!    consecutive-day invocations diverge.

use std::collections::HashMap;

use kino_core::constants::{
    FINAL_LIST_PATTERN, HIDDEN_GEMS_RATING_THRESHOLD, TOP_TRENDING_QUARTILE, TRENDING_RESULT_COUNT,
};
use kino_core::title::TitleSummary;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Per-provider trending entry consumed by [`aggregate`].
///
/// Built by each provider client's `trending_*` method. The aggregator never
/// constructs these directly outside tests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderItem {
    /// Catalog-facing summary (title, year, poster, etc.). `id` doubles as
    /// the cross-provider dedup key per PRD §F-004 step 2.
    pub summary: TitleSummary,
    /// 0-indexed position in the provider's trending response (0 = top).
    pub rank: usize,
    /// TMDB-style popularity score; populated by TMDB only. Used to compute
    /// the median that gates Hidden-Gems eligibility (PRD §F-004 step 5).
    pub popularity: Option<f64>,
    /// 0..10 community rating; populated by TMDB (`vote_average`) and TVDB
    /// (`score`). Trakt trending does not surface a per-item rating in v1.
    pub rating: Option<f64>,
}

/// Per-provider input weights from PRD §F-004 step 4.
const TMDB_WEIGHT: f64 = 0.35;
const TRAKT_WEIGHT: f64 = 0.45;
const TVDB_WEIGHT: f64 = 0.20;
const NEUTRAL_RANK: f64 = 0.5;

/// Run the locked F-004 algorithm against the per-provider trending inputs.
///
/// `today` is the UTC date used to seed the daily shuffle (PRD step 7);
/// `install_id` is the bootstrapped `settings.install_id` (PRD §F-002,
/// ADR-023). Both feed the `SHA256` hash that becomes the `ChaCha20` seed.
///
/// The returned list contains at most [`TRENDING_RESULT_COUNT`] items.
/// Fewer is fine — when the aggregate inputs are smaller than the cap,
/// every item is included.
#[must_use]
#[allow(clippy::similar_names)] // `tmdb` / `tvdb` are PRD-locked provider names.
pub fn aggregate(
    tmdb: Vec<ProviderItem>,
    trakt: Vec<ProviderItem>,
    tvdb: Vec<ProviderItem>,
    install_id: &str,
    today_utc: &str,
) -> Vec<TitleSummary> {
    let merged = merge_by_id(tmdb, trakt, tvdb);
    let (top, gems) = split_pools(&merged);
    let interleaved = interleave(top, gems, TRENDING_RESULT_COUNT);
    let mut summaries: Vec<TitleSummary> = interleaved.into_iter().map(|m| m.summary).collect();
    let seed = seed_for_day(today_utc, install_id);
    let mut rng = ChaCha20Rng::from_seed(seed);
    summaries.shuffle(&mut rng);
    summaries
}

/// `SHA256` of `"{date} {install_id}"` truncated to 32 bytes for
/// `ChaCha20`. `SHA256` already produces exactly 32 bytes, so the array
/// conversion is infallible.
#[must_use]
pub fn seed_for_day(today_utc: &str, install_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(today_utc.as_bytes());
    hasher.update(b" ");
    hasher.update(install_id.as_bytes());
    let digest = hasher.finalize();
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&digest);
    seed
}

/// Intermediate per-title shape after merging. Holds the score, rating, and
/// per-provider ranks needed by the pool-split step.
#[derive(Debug, Clone)]
struct Merged {
    summary: TitleSummary,
    tmdb_rank: Option<usize>,
    trakt_rank: Option<usize>,
    tvdb_rank: Option<usize>,
    popularity: Option<f64>,
    rating: Option<f64>,
}

impl Merged {
    #[allow(clippy::similar_names)] // PRD-locked provider names.
    fn weighted_score(&self, tmdb_total: usize, trakt_total: usize, tvdb_total: usize) -> f64 {
        let tmdb_n = self
            .tmdb_rank
            .map_or(NEUTRAL_RANK, |r| normalize_rank(r, tmdb_total));
        let trakt_n = self
            .trakt_rank
            .map_or(NEUTRAL_RANK, |r| normalize_rank(r, trakt_total));
        let tvdb_n = self
            .tvdb_rank
            .map_or(NEUTRAL_RANK, |r| normalize_rank(r, tvdb_total));
        TRAKT_WEIGHT * trakt_n + TMDB_WEIGHT * tmdb_n + TVDB_WEIGHT * tvdb_n
    }
}

fn normalize_rank(rank: usize, total: usize) -> f64 {
    // 1-element provider input would otherwise yield NaN. Treat single-item
    // inputs as "best": rank 0 → 0.0.
    if total <= 1 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let r = rank as f64 / (total - 1) as f64;
    r.clamp(0.0, 1.0)
}

/// Step 2: deduplicate by [`TitleSummary::id`], folding ranks/rating/popularity
/// from whichever providers contributed the same id. Preserves first-seen
/// title metadata (we prefer TMDB's poster + rating when present, but the
/// id is the dedup key, not the title).
#[allow(clippy::similar_names)] // PRD-locked provider names.
fn merge_by_id(
    tmdb: Vec<ProviderItem>,
    trakt: Vec<ProviderItem>,
    tvdb: Vec<ProviderItem>,
) -> Vec<Merged> {
    enum ProviderSlot {
        Tmdb,
        Trakt,
        Tvdb,
    }
    let mut map: HashMap<String, Merged> = HashMap::new();
    let mut fold = |items: Vec<ProviderItem>, slot: ProviderSlot| {
        for item in items {
            let entry = map
                .entry(item.summary.id.clone())
                .or_insert_with(|| Merged {
                    summary: item.summary.clone(),
                    tmdb_rank: None,
                    trakt_rank: None,
                    tvdb_rank: None,
                    popularity: item.popularity,
                    rating: item.rating,
                });
            match slot {
                ProviderSlot::Tmdb => entry.tmdb_rank = Some(item.rank),
                ProviderSlot::Trakt => entry.trakt_rank = Some(item.rank),
                ProviderSlot::Tvdb => entry.tvdb_rank = Some(item.rank),
            }
            if entry.popularity.is_none() {
                entry.popularity = item.popularity;
            }
            if entry.rating.is_none() {
                entry.rating = item.rating;
            }
        }
    };
    fold(tmdb, ProviderSlot::Tmdb);
    fold(trakt, ProviderSlot::Trakt);
    fold(tvdb, ProviderSlot::Tvdb);
    map.into_values().collect()
}

/// Step 5: split merged candidates into Top-Trending and Hidden-Gems.
///
/// Top-Trending = the items whose weighted score lands in the
/// [`TOP_TRENDING_QUARTILE`] quantile (best scores first; lower = better).
/// Hidden-Gems = items NOT in Top-Trending, with `rating` strictly above
/// [`HIDDEN_GEMS_RATING_THRESHOLD`] AND `popularity_rank` strictly below
/// the median `popularity_rank` of items with a known popularity.
///
/// Both returned lists are sorted by weighted score ascending (best first).
#[allow(clippy::similar_names)] // PRD-locked provider names.
fn split_pools(merged: &[Merged]) -> (Vec<Merged>, Vec<Merged>) {
    if merged.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let tmdb_n = merged.iter().filter(|m| m.tmdb_rank.is_some()).count();
    let trakt_n = merged.iter().filter(|m| m.trakt_rank.is_some()).count();
    let tvdb_n = merged.iter().filter(|m| m.tvdb_rank.is_some()).count();

    // Pair each item with its score and sort by score asc (lower = better).
    let mut scored: Vec<(f64, Merged)> = merged
        .iter()
        .map(|m| (m.weighted_score(tmdb_n, trakt_n, tvdb_n), m.clone()))
        .collect();
    scored.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.summary.id.cmp(&b.1.summary.id))
    });

    let n = scored.len();
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let top_cutoff = ((n as f64 * TOP_TRENDING_QUARTILE).ceil() as usize)
        .max(1)
        .min(n);

    // Popularity-rank median: among items whose popularity is known, rank
    // them best-first (highest popularity = rank 0), then take median(rank).
    let popularity_ranks = popularity_rank_map(merged);
    let median_pop_rank = median_value(popularity_ranks.values().copied());

    let mut top: Vec<Merged> = Vec::with_capacity(top_cutoff);
    let mut gems: Vec<Merged> = Vec::with_capacity(n.saturating_sub(top_cutoff));
    for (i, (_, m)) in scored.into_iter().enumerate() {
        if i < top_cutoff {
            top.push(m);
        } else if is_hidden_gem(&m, &popularity_ranks, median_pop_rank) {
            gems.push(m);
        }
    }
    (top, gems)
}

/// Build a map `id -> popularity_rank` (rank 0 = most popular). Items with
/// no popularity are absent — those titles can't be Hidden Gems by
/// definition.
fn popularity_rank_map(merged: &[Merged]) -> HashMap<String, usize> {
    let mut with_pop: Vec<(&String, f64)> = merged
        .iter()
        .filter_map(|m| m.popularity.map(|p| (&m.summary.id, p)))
        .collect();
    with_pop.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    with_pop
        .into_iter()
        .enumerate()
        .map(|(rank, (id, _))| (id.clone(), rank))
        .collect()
}

fn is_hidden_gem(
    m: &Merged,
    pop_ranks: &HashMap<String, usize>,
    median_pop_rank: Option<f64>,
) -> bool {
    let Some(rating) = m.rating else { return false };
    if rating <= HIDDEN_GEMS_RATING_THRESHOLD {
        return false;
    }
    let Some(median) = median_pop_rank else {
        return false;
    };
    let Some(rank) = pop_ranks.get(&m.summary.id) else {
        return false;
    };
    #[allow(clippy::cast_precision_loss)]
    let r = *rank as f64;
    r < median
}

fn median_value<I: IntoIterator<Item = usize>>(iter: I) -> Option<f64> {
    let mut values: Vec<usize> = iter.into_iter().collect();
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    #[allow(clippy::cast_precision_loss)]
    let result = if values.len().is_multiple_of(2) {
        f64::midpoint(values[mid - 1] as f64, values[mid] as f64)
    } else {
        values[mid] as f64
    };
    Some(result)
}

/// Step 6: alternate Top and Gems per [`FINAL_LIST_PATTERN`] until `count`
/// items are collected. If a pool runs out, fill from the other.
fn interleave(mut top: Vec<Merged>, mut gems: Vec<Merged>, count: usize) -> Vec<Merged> {
    let mut out: Vec<Merged> = Vec::with_capacity(count);
    let mut top_iter = top.drain(..);
    let mut gem_iter = gems.drain(..);
    let mut pattern_idx = 0;
    while out.len() < count {
        let want_top = FINAL_LIST_PATTERN[pattern_idx % FINAL_LIST_PATTERN.len()];
        pattern_idx += 1;
        let picked = if want_top {
            top_iter.next().or_else(|| gem_iter.next())
        } else {
            gem_iter.next().or_else(|| top_iter.next())
        };
        match picked {
            Some(m) => out.push(m),
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kino_core::title::TitleKind;

    fn item(id: &str, rank: usize, popularity: Option<f64>, rating: Option<f64>) -> ProviderItem {
        ProviderItem {
            summary: TitleSummary {
                id: id.to_string(),
                kind: TitleKind::Movie,
                title: format!("Title {id}"),
                year: None,
                poster: None,
                rating,
            },
            rank,
            popularity,
            rating,
        }
    }

    #[test]
    #[allow(clippy::similar_names)] // PRD-locked provider names.
    fn merge_dedupes_same_id_across_providers() {
        let tmdb = vec![
            item("tt1", 0, Some(100.0), Some(8.0)),
            item("tt2", 1, Some(50.0), Some(6.0)),
        ];
        let trakt = vec![item("tt1", 0, None, None)];
        let tvdb = vec![item("tt3", 0, None, None)];
        let merged = merge_by_id(tmdb, trakt, tvdb);
        assert_eq!(merged.len(), 3);
        let by_id: HashMap<_, _> = merged.iter().map(|m| (m.summary.id.clone(), m)).collect();
        let tt1 = by_id.get("tt1").unwrap();
        assert!(tt1.tmdb_rank.is_some());
        assert!(tt1.trakt_rank.is_some());
        assert_eq!(tt1.popularity, Some(100.0));
    }

    #[test]
    fn weighted_score_matches_locked_formula() {
        // rank 0 on each provider with total 2 normalizes to 0.0 each →
        // weighted = 0 across all weights.
        let m = Merged {
            summary: TitleSummary {
                id: "x".into(),
                kind: TitleKind::Movie,
                title: "x".into(),
                year: None,
                poster: None,
                rating: None,
            },
            tmdb_rank: Some(0),
            trakt_rank: Some(0),
            tvdb_rank: Some(0),
            popularity: None,
            rating: None,
        };
        let s = m.weighted_score(2, 2, 2);
        assert!(s.abs() < 1e-9, "expected 0.0, got {s}");

        // Missing provider rank → 0.5 neutral.
        let m2 = Merged {
            summary: TitleSummary {
                id: "y".into(),
                kind: TitleKind::Movie,
                title: "y".into(),
                year: None,
                poster: None,
                rating: None,
            },
            tmdb_rank: None,
            trakt_rank: None,
            tvdb_rank: None,
            popularity: None,
            rating: None,
        };
        let s2 = m2.weighted_score(0, 0, 0);
        // 0.45*0.5 + 0.35*0.5 + 0.20*0.5 = 0.5
        assert!((s2 - 0.5).abs() < 1e-9, "expected 0.5, got {s2}");
    }

    #[test]
    fn split_pools_top_quartile_and_hidden_gems() {
        // Eight items: TMDB-ranked 0..7. Each with a unique rating and
        // popularity. Top quartile = ceil(8 * 0.25) = 2 items.
        let mut tmdb = Vec::new();
        for i in 0..8 {
            #[allow(clippy::cast_precision_loss)]
            tmdb.push(item(
                &format!("tt{i}"),
                i,
                Some(100.0 - i as f64),
                Some(if i >= 4 { 8.0 } else { 5.0 }),
            ));
        }
        let merged = merge_by_id(tmdb, vec![], vec![]);
        let (top, gems) = split_pools(&merged);
        assert_eq!(top.len(), 2);
        // Top two should be the best-ranked TMDB entries (tt0, tt1).
        let top_ids: Vec<_> = top.iter().map(|m| m.summary.id.clone()).collect();
        assert!(top_ids.contains(&"tt0".to_string()));
        assert!(top_ids.contains(&"tt1".to_string()));
        // Hidden gems must have rating > 7.5 AND popularity_rank below median.
        // Items 4-7 have rating 8.0; tt0/tt1 are in top so they're excluded.
        // Popularity rank: tt0=0, tt1=1, ..., tt7=7. Median of 8 is 3.5.
        // So gems with pop_rank < 3.5: tt0, tt1, tt2, tt3. tt0/tt1 are
        // already top. tt2/tt3 rating is 5.0 so not gems. Among gems pool
        // (tt4..tt7 by rating), pop_ranks are 4..7 all >= 3.5 → no gems.
        assert_eq!(gems.len(), 0);
    }

    #[test]
    fn split_pools_finds_gems_with_high_rating_and_low_pop_rank() {
        // tt0/tt1 = top quartile (lowest score). tt2 has high rating + high
        // popularity (low rank) → gem. Others are filler.
        let mut tmdb = Vec::new();
        for i in 0..8 {
            tmdb.push(item(
                &format!("tt{i}"),
                i,
                // popularity descending so tt0 most popular, tt7 least.
                #[allow(clippy::cast_precision_loss)]
                Some(100.0 - i as f64),
                Some(if i == 2 || i == 3 { 8.0 } else { 5.0 }),
            ));
        }
        // tt2 has TMDB rank 2 → not in top quartile (cutoff 2 = positions 0/1).
        // tt2 rating 8.0 > 7.5; pop_rank for tt2 = 2 < median(3.5). → gem ✓.
        // tt3 rating 8.0, pop_rank 3 < 3.5. → gem ✓.
        let merged = merge_by_id(tmdb, vec![], vec![]);
        let (_top, gems) = split_pools(&merged);
        let gem_ids: Vec<_> = gems.iter().map(|m| m.summary.id.clone()).collect();
        assert!(gem_ids.contains(&"tt2".to_string()), "gems = {gem_ids:?}");
        assert!(gem_ids.contains(&"tt3".to_string()), "gems = {gem_ids:?}");
    }

    #[test]
    fn interleave_follows_locked_pattern_ttggg() {
        let top: Vec<Merged> = (0..10)
            .map(|i| Merged {
                summary: TitleSummary {
                    id: format!("T{i}"),
                    kind: TitleKind::Movie,
                    title: format!("T{i}"),
                    year: None,
                    poster: None,
                    rating: None,
                },
                tmdb_rank: None,
                trakt_rank: None,
                tvdb_rank: None,
                popularity: None,
                rating: None,
            })
            .collect();
        let gems: Vec<Merged> = (0..10)
            .map(|i| Merged {
                summary: TitleSummary {
                    id: format!("G{i}"),
                    kind: TitleKind::Movie,
                    title: format!("G{i}"),
                    year: None,
                    poster: None,
                    rating: None,
                },
                tmdb_rank: None,
                trakt_rank: None,
                tvdb_rank: None,
                popularity: None,
                rating: None,
            })
            .collect();
        let out = interleave(top, gems, 10);
        let ids: Vec<_> = out.iter().map(|m| m.summary.id.clone()).collect();
        // Pattern [T,T,T,G,G] over 10 slots: TTT GG TTT GG.
        assert_eq!(
            ids,
            vec!["T0", "T1", "T2", "G0", "G1", "T3", "T4", "T5", "G2", "G3"]
        );
    }

    #[test]
    fn interleave_fills_from_other_pool_when_empty() {
        let top: Vec<Merged> = (0..3)
            .map(|i| Merged {
                summary: TitleSummary {
                    id: format!("T{i}"),
                    kind: TitleKind::Movie,
                    title: format!("T{i}"),
                    year: None,
                    poster: None,
                    rating: None,
                },
                tmdb_rank: None,
                trakt_rank: None,
                tvdb_rank: None,
                popularity: None,
                rating: None,
            })
            .collect();
        let gems: Vec<Merged> = (0..6)
            .map(|i| Merged {
                summary: TitleSummary {
                    id: format!("G{i}"),
                    kind: TitleKind::Movie,
                    title: format!("G{i}"),
                    year: None,
                    poster: None,
                    rating: None,
                },
                tmdb_rank: None,
                trakt_rank: None,
                tvdb_rank: None,
                popularity: None,
                rating: None,
            })
            .collect();
        let out = interleave(top, gems, 9);
        assert_eq!(out.len(), 9);
        // First 3 slots want T, satisfied by T0/T1/T2. Next 2 want G:
        // G0/G1. Then 3 more T slots — top is exhausted, falls through to
        // gems: G2/G3/G4. Last slot wants G: G5.
        let ids: Vec<_> = out.iter().map(|m| m.summary.id.clone()).collect();
        assert_eq!(
            ids,
            vec!["T0", "T1", "T2", "G0", "G1", "G2", "G3", "G4", "G5"]
        );
    }

    #[test]
    fn seed_for_day_is_deterministic() {
        let a = seed_for_day("2025-04-01", "install-x");
        let b = seed_for_day("2025-04-01", "install-x");
        assert_eq!(a, b);
        let c = seed_for_day("2025-04-02", "install-x");
        assert_ne!(a, c);
        let d = seed_for_day("2025-04-01", "install-y");
        assert_ne!(a, d);
    }

    #[test]
    fn aggregate_returns_at_most_trending_result_count_items() {
        let mut tmdb = Vec::new();
        for i in 0..80 {
            tmdb.push(item(
                &format!("tt{i}"),
                i,
                #[allow(clippy::cast_precision_loss)]
                Some(100.0 - i as f64),
                Some(7.0),
            ));
        }
        let out = aggregate(tmdb, vec![], vec![], "install", "2025-01-01");
        assert!(out.len() <= TRENDING_RESULT_COUNT);
    }

    #[test]
    fn aggregate_same_day_same_install_is_identical() {
        let make_inputs = || {
            let mut v = Vec::new();
            for i in 0..40 {
                v.push(item(
                    &format!("tt{i}"),
                    i,
                    #[allow(clippy::cast_precision_loss)]
                    Some(100.0 - i as f64),
                    Some(if i % 2 == 0 { 8.0 } else { 5.0 }),
                ));
            }
            v
        };
        let a = aggregate(make_inputs(), vec![], vec![], "install-x", "2025-04-01");
        let b = aggregate(make_inputs(), vec![], vec![], "install-x", "2025-04-01");
        assert_eq!(a, b);
    }

    #[test]
    fn aggregate_consecutive_days_have_low_kendall_tau() {
        let make_inputs = || {
            let mut v = Vec::new();
            for i in 0..TRENDING_RESULT_COUNT {
                v.push(item(
                    &format!("tt{i}"),
                    i,
                    #[allow(clippy::cast_precision_loss)]
                    Some(200.0 - i as f64),
                    Some(if i % 3 == 0 { 8.0 } else { 5.0 }),
                ));
            }
            v
        };
        let day1 = aggregate(make_inputs(), vec![], vec![], "install-x", "2025-04-01");
        let day2 = aggregate(make_inputs(), vec![], vec![], "install-x", "2025-04-02");
        assert_eq!(day1.len(), day2.len());
        let tau = kendall_tau_by_id(&day1, &day2);
        assert!(
            tau.abs() < 0.7,
            "consecutive-day tau should be small, got {tau}"
        );
    }

    #[test]
    fn aggregate_different_install_ids_differ_on_same_day() {
        let make_inputs = || {
            let mut v = Vec::new();
            for i in 0..40 {
                v.push(item(
                    &format!("tt{i}"),
                    i,
                    #[allow(clippy::cast_precision_loss)]
                    Some(100.0 - i as f64),
                    Some(7.0),
                ));
            }
            v
        };
        let a = aggregate(make_inputs(), vec![], vec![], "install-A", "2025-04-01");
        let b = aggregate(make_inputs(), vec![], vec![], "install-B", "2025-04-01");
        assert_ne!(a, b);
    }

    /// Kendall tau between two orderings of the same id set, where we compare
    /// the position of each id in `a` vs in `b`. Returns a value in
    /// `[-1.0, 1.0]` where 1.0 = identical ordering, 0.0 = uncorrelated,
    /// -1.0 = reversed.
    fn kendall_tau_by_id(a: &[TitleSummary], b: &[TitleSummary]) -> f64 {
        let pos_b: HashMap<&str, usize> = b
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.as_str(), i))
            .collect();
        let pairs: Vec<(usize, usize)> = a
            .iter()
            .enumerate()
            .filter_map(|(i, s)| pos_b.get(s.id.as_str()).map(|&j| (i, j)))
            .collect();
        let n = pairs.len();
        if n < 2 {
            return 1.0;
        }
        let mut concordant: i64 = 0;
        let mut discordant: i64 = 0;
        for i in 0..n {
            for j in (i + 1)..n {
                let (ai, aj) = (pairs[i].0, pairs[j].0);
                let (bi, bj) = (pairs[i].1, pairs[j].1);
                let cmp_a = ai.cmp(&aj);
                let cmp_b = bi.cmp(&bj);
                if cmp_a == cmp_b {
                    concordant += 1;
                } else if cmp_a != std::cmp::Ordering::Equal && cmp_b != std::cmp::Ordering::Equal {
                    discordant += 1;
                }
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let denom = (n * (n - 1) / 2) as f64;
        #[allow(clippy::cast_precision_loss)]
        let num = (concordant - discordant) as f64;
        num / denom
    }
}
