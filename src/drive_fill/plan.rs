/// Upper bound on DP cells (items × capacity buckets). Keeps planning well
/// under a second and a few MB of memory however large the drive is.
const MAX_DP_CELLS: u64 = 40_000_000;

/// Picks the subset of `weights` whose total is as large as possible without
/// exceeding `capacity` (0/1 knapsack with value = weight). Returns indices
/// into `weights`, ascending.
///
/// Solved exactly by dynamic programming over capacity *buckets*: each
/// weight is rounded **up** to whole buckets, so whatever the DP picks is
/// guaranteed to fit for real, at the cost of a small amount of slack per
/// chosen item. A largest-first greedy pass is computed too and the better
/// of the two (by exact bytes) wins, covering the cases where rounding
/// makes the DP too conservative.
pub fn choose_best_fit(weights: &[u64], capacity: u64) -> Vec<usize> {
    let candidates: Vec<usize> = (0..weights.len())
        .filter(|&i| weights[i] <= capacity)
        .collect();
    let all_total: u64 = candidates.iter().map(|&i| weights[i]).sum();
    if all_total <= capacity {
        return candidates;
    }

    let dp = dp_fit(weights, &candidates, capacity);
    let greedy = greedy_fit(weights, &candidates, capacity);
    let total = |set: &[usize]| set.iter().map(|&i| weights[i]).sum::<u64>();
    let mut best = if total(&greedy) > total(&dp) {
        greedy
    } else {
        dp
    };
    best.sort_unstable();
    best
}

fn greedy_fit(weights: &[u64], candidates: &[usize], capacity: u64) -> Vec<usize> {
    let mut order = candidates.to_vec();
    order.sort_by_key(|&i| std::cmp::Reverse(weights[i]));
    let mut left = capacity;
    let mut chosen = Vec::new();
    for i in order {
        if weights[i] <= left {
            left -= weights[i];
            chosen.push(i);
        }
    }
    chosen
}

fn dp_fit(weights: &[u64], candidates: &[usize], capacity: u64) -> Vec<usize> {
    let n = candidates.len() as u64;
    if n == 0 {
        return Vec::new();
    }
    let buckets = (MAX_DP_CELLS / n).clamp(1, capacity.max(1));
    let unit = capacity.div_ceil(buckets).max(1);
    let cap = (capacity / unit) as usize;
    let scaled: Vec<usize> = candidates
        .iter()
        .map(|&i| weights[i].div_ceil(unit) as usize)
        .collect();

    // rows[k] = bucket totals reachable using the first k candidates.
    let words = cap / 64 + 1;
    let mut rows: Vec<Vec<u64>> = Vec::with_capacity(candidates.len() + 1);
    let mut first = vec![0u64; words];
    first[0] = 1;
    rows.push(first);
    for &w in &scaled {
        let prev = rows.last().unwrap();
        let mut next = prev.clone();
        if w <= cap {
            or_shifted(&mut next, prev, w);
            // Clear anything shifted past `cap`.
            let extra = (words * 64) - (cap + 1);
            if extra > 0 {
                next[words - 1] &= u64::MAX >> extra;
            }
        }
        rows.push(next);
    }

    let last = rows.last().unwrap();
    let mut j = (0..=cap).rev().find(|&j| bit(last, j)).unwrap_or(0);
    let mut chosen = Vec::new();
    for k in (0..scaled.len()).rev() {
        if !bit(&rows[k], j) {
            // Reachable with item k but not without it, so it was taken.
            chosen.push(candidates[k]);
            j -= scaled[k];
        }
    }
    chosen
}

fn bit(row: &[u64], j: usize) -> bool {
    row[j / 64] >> (j % 64) & 1 == 1
}

/// `dst |= src << shift` over a little-endian bitset.
fn or_shifted(dst: &mut [u64], src: &[u64], shift: usize) {
    let word_shift = shift / 64;
    let bit_shift = shift % 64;
    for i in (word_shift..dst.len()).rev() {
        let from = i - word_shift;
        let mut v = src[from] << bit_shift;
        if bit_shift > 0 && from > 0 {
            v |= src[from - 1] >> (64 - bit_shift);
        }
        dst[i] |= v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total(weights: &[u64], set: &[usize]) -> u64 {
        set.iter().map(|&i| weights[i]).sum()
    }

    #[test]
    fn takes_everything_when_it_all_fits() {
        assert_eq!(choose_best_fit(&[1, 2, 3], 10), vec![0, 1, 2]);
    }

    #[test]
    fn skips_items_bigger_than_the_whole_capacity() {
        assert_eq!(choose_best_fit(&[50, 3, 4], 10), vec![1, 2]);
    }

    #[test]
    fn beats_largest_first_greedy() {
        // Greedy takes 6 and then nothing else fits (6+5 > 10): total 6.
        // The optimum is 5 + 5 = 10.
        let weights = [6, 5, 5];
        let chosen = choose_best_fit(&weights, 10);
        assert_eq!(total(&weights, &chosen), 10);
    }

    #[test]
    fn never_exceeds_capacity_with_large_byte_sizes() {
        // Drive-sized numbers, so the DP has to bucket.
        let gb = 1_000_000_000u64;
        let weights: Vec<u64> = (1..=60).map(|i| i * 7 * gb + i * 12_345).collect();
        let capacity = 1_000 * gb;
        let chosen = choose_best_fit(&weights, capacity);
        let used = total(&weights, &chosen);
        assert!(used <= capacity);
        // Within 1% of full.
        assert!(used as f64 >= capacity as f64 * 0.99, "used {used}");
    }

    #[test]
    fn finds_the_exact_optimum_on_small_inputs() {
        // Brute-force cross-check over every subset.
        let weights = [23, 31, 29, 44, 53, 38, 63, 85, 89, 82];
        let capacity = 165;
        let best = (0u32..1 << weights.len())
            .map(|mask| {
                (0..weights.len())
                    .filter(|i| mask >> i & 1 == 1)
                    .map(|i| weights[i])
                    .sum::<u64>()
            })
            .filter(|&t| t <= capacity)
            .max()
            .unwrap();
        let chosen = choose_best_fit(&weights, capacity);
        assert_eq!(total(&weights, &chosen), best);
    }
}
