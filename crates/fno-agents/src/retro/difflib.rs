//! Python's `difflib.SequenceMatcher(None, a, b).ratio()` over characters.
//!
//! The sweep's fuzzy subject match compares this ratio against a fixed
//! floor, so it must agree with the Python value, autojunk heuristic
//! included: in a sequence of 200 or more items, an element appearing more
//! than `n / 100 + 1` times is dropped from the match index.

use std::collections::HashMap;

/// `SequenceMatcher(None, a, b).ratio()`.
pub fn ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    let matches: usize = matching_blocks(&a, &b).iter().map(|&(_, _, n)| n).sum();
    2.0 * matches as f64 / total as f64
}

fn b2j(b: &[char]) -> HashMap<char, Vec<usize>> {
    let mut index: HashMap<char, Vec<usize>> = HashMap::new();
    for (j, &ch) in b.iter().enumerate() {
        index.entry(ch).or_default().push(j);
    }
    let n = b.len();
    if n >= 200 {
        let ntest = n / 100 + 1;
        index.retain(|_, idxs| idxs.len() <= ntest);
    }
    index
}

fn longest_match(
    a: &[char],
    b: &[char],
    index: &HashMap<char, Vec<usize>>,
    (alo, ahi, blo, bhi): (usize, usize, usize, usize),
) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for i in alo..ahi {
        let mut next: HashMap<usize, usize> = HashMap::new();
        if let Some(js) = index.get(&a[i]) {
            for &j in js {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                let k = j
                    .checked_sub(1)
                    .and_then(|p| j2len.get(&p))
                    .copied()
                    .unwrap_or(0)
                    + 1;
                next.insert(j, k);
                if k > bestsize {
                    besti = i + 1 - k;
                    bestj = j + 1 - k;
                    bestsize = k;
                }
            }
        }
        j2len = next;
    }
    // No junk set, so only the non-junk extension loops can fire; they
    // recover matches through elements autojunk dropped from the index.
    while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
        besti -= 1;
        bestj -= 1;
        bestsize += 1;
    }
    while besti + bestsize < ahi
        && bestj + bestsize < bhi
        && a[besti + bestsize] == b[bestj + bestsize]
    {
        bestsize += 1;
    }
    (besti, bestj, bestsize)
}

fn matching_blocks(a: &[char], b: &[char]) -> Vec<(usize, usize, usize)> {
    let index = b2j(b);
    let mut queue = vec![(0, a.len(), 0, b.len())];
    let mut blocks = Vec::new();
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest_match(a, b, &index, (alo, ahi, blo, bhi));
        if k > 0 {
            blocks.push((i, j, k));
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    blocks.sort_unstable();
    // Collapse adjacent blocks, as Python does before counting.
    let mut merged: Vec<(usize, usize, usize)> = Vec::new();
    for (i, j, k) in blocks {
        if let Some(last) = merged.last_mut() {
            if last.0 + last.2 == i && last.1 + last.2 == j {
                last.2 += k;
                continue;
            }
        }
        merged.push((i, j, k));
    }
    merged
}
