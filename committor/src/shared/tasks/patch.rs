//! Insertion-aware commit patch: a greedy copy/insert delta encoder (xdelta style).
//!
//! 1. Match 16-byte windows of `new` against `old` (same offset, else hashed blocks) and
//!    extend each match both ways.
//! 2. Topologically sort moves for in-place apply; drop moves in cycles.
//! 3. Emit literals for uncovered bytes.
//!
//! `old` is zero-padded to `new.len()` to match the decoder's resize.
//!
//! Layout (u32 little-endian): `new_len | moves | writes | (src, dst, len)* | (dst, len)* | bytes`.
//! Apply: resize to `max(old_len, new_len)`, run moves in order (memmove), copy in the writes,
//! truncate to `new_len`.

use std::collections::HashMap;

const WINDOW: usize = 16;
const BASE: u64 = 257;
const BASE_POW: u64 = BASE.wrapping_pow(WINDOW as u32 - 1);

#[derive(Clone, Copy)]
struct Span {
    src: usize,
    dst: usize,
    len: usize,
}

fn hash(window: &[u8]) -> u64 {
    window.iter().fold(0, |hash, &byte| {
        hash.wrapping_mul(BASE).wrapping_add(byte as u64)
    })
}

/// Slides the window hash one byte: drops `outgoing`, appends `incoming`.
fn roll(hash: u64, outgoing: u8, incoming: u8) -> u64 {
    hash.wrapping_sub((outgoing as u64).wrapping_mul(BASE_POW))
        .wrapping_mul(BASE)
        .wrapping_add(incoming as u64)
}

fn find_copies(old: &[u8], new: &[u8]) -> Vec<Span> {
    let mut blocks = HashMap::new();
    for (block, window) in old.chunks_exact(WINDOW).enumerate() {
        blocks.entry(hash(window)).or_insert(block * WINDOW);
    }

    let mut copies = Vec::new();
    let mut literal_start = 0;
    let mut position = 0;
    let mut window_hash = new.get(..WINDOW).map_or(0, hash);
    while position + WINDOW <= new.len() {
        let window = &new[position..position + WINDOW];
        let src = if old.get(position..position + WINDOW) == Some(window) {
            Some(position)
        } else {
            blocks
                .get(&window_hash)
                .copied()
                .filter(|&src| &old[src..src + WINDOW] == window)
        };

        let Some(src) = src else {
            if let Some(&incoming) = new.get(position + WINDOW) {
                window_hash = roll(window_hash, new[position], incoming);
            }
            position += 1;
            continue;
        };

        let mut copy = Span { src, dst: position, len: WINDOW };
        while copy.dst > literal_start && copy.src > 0 && old[copy.src - 1] == new[copy.dst - 1] {
            copy.src -= 1;
            copy.dst -= 1;
            copy.len += 1;
        }
        while copy.src + copy.len < old.len()
            && copy.dst + copy.len < new.len()
            && old[copy.src + copy.len] == new[copy.dst + copy.len]
        {
            copy.len += 1;
        }
        copies.push(copy);
        position = copy.dst + copy.len;
        literal_start = position;
        window_hash = new.get(position..position + WINDOW).map_or(0, hash);
    }
    copies
}

/// Returns the indices of the moves to run, in order. `moves` must be sorted by `dst`.
fn order_moves(moves: &[Span]) -> Vec<usize> {
    let mut overwrites = vec![Vec::new(); moves.len()];
    let mut readers_left = vec![0; moves.len()];
    for (reader, read) in moves.iter().enumerate() {
        let first = moves.partition_point(|write| write.dst + write.len <= read.src);
        for writer in first..moves.len() {
            if moves[writer].dst >= read.src + read.len {
                break;
            }
            if writer != reader {
                overwrites[reader].push(writer);
                readers_left[writer] += 1;
            }
        }
    }

    let mut ready: Vec<usize> = (0..moves.len()).filter(|&node| readers_left[node] == 0).collect();
    let mut done = vec![false; moves.len()];
    let mut order = Vec::new();
    let mut next_stuck = 0;
    loop {
        let node = if let Some(node) = ready.pop() {
            order.push(node);
            node
        } else if let Some(stuck) = (next_stuck..moves.len()).find(|&node| !done[node]) {
            next_stuck = stuck;
            stuck
        } else {
            break;
        };
        done[node] = true;
        for &writer in &overwrites[node] {
            readers_left[writer] -= 1;
            if readers_left[writer] == 0 && !done[writer] {
                ready.push(writer);
            }
        }
    }
    order
}

/// Encodes `new` as a patch against `old`, or `None` if the full state is smaller.
pub(crate) fn compute_patch(old: &[u8], new: &[u8]) -> Option<Vec<u8>> {
    let mut padded = old.to_vec();
    padded.resize(old.len().max(new.len()), 0);
    let (mut kept, moves): (Vec<Span>, Vec<Span>) =
        find_copies(&padded, new).into_iter().partition(|copy| copy.src == copy.dst);
    let order = order_moves(&moves);
    kept.extend(order.iter().map(|&node| moves[node]));
    kept.sort_by_key(|copy| copy.dst);

    let mut writes = Vec::new();
    let mut end = 0;
    for copy in &kept {
        if copy.dst > end {
            writes.push(end..copy.dst);
        }
        end = copy.dst + copy.len;
    }
    if end < new.len() {
        writes.push(end..new.len());
    }

    let mut words = vec![new.len(), order.len(), writes.len()];
    for &node in &order {
        words.extend([moves[node].src, moves[node].dst, moves[node].len]);
    }
    for write in &writes {
        words.extend([write.start, write.len()]);
    }
    let mut patch: Vec<u8> = words.iter().flat_map(|&word| (word as u32).to_le_bytes()).collect();
    for write in writes {
        patch.extend_from_slice(&new[write]);
    }
    (patch.len() < new.len()).then_some(patch)
}

#[cfg(test)]
mod tests {
    use rand::{Rng, SeedableRng, rngs::StdRng};

    use super::compute_patch;

    fn apply_patch(old: &[u8], patch: &[u8]) -> Vec<u8> {
        let word = |index: usize| {
            u32::from_le_bytes(patch[4 * index..4 * index + 4].try_into().unwrap()) as usize
        };
        let (len, moves, writes) = (word(0), word(1), word(2));
        let mut data = old.to_vec();
        data.resize(len.max(old.len()), 0);
        for index in (3..3 + 3 * moves).step_by(3) {
            data.copy_within(word(index)..word(index) + word(index + 2), word(index + 1));
        }
        let mut literal = 4 * (3 + 3 * moves + 2 * writes);
        for index in (3 + 3 * moves..3 + 3 * moves + 2 * writes).step_by(2) {
            let (dst, len) = (word(index), word(index + 1));
            data[dst..dst + len].copy_from_slice(&patch[literal..literal + len]);
            literal += len;
        }
        data.truncate(len);
        data
    }

    #[test]
    fn test_compute_patch() {
        let mut rng = StdRng::seed_from_u64(7);
        let old: Vec<u8> = (0..4096).map(|_| rng.random()).collect();
        let splice = |at: usize, remove: usize, insert: &[u8]| {
            let mut data = old.clone();
            data.splice(at..at + remove, insert.iter().copied());
            data
        };
        let mut edited = splice(100, 0, &[1, 2, 3]);
        edited[3000] ^= 1;

        for (new, max_len) in [
            (old.clone(), 12),
            (splice(0, 0, &[1]), 33),
            (splice(2000, 50, &[]), 24),
            (edited, 56),
            (splice(4096, 0, &[9; 10]), 30),
            (splice(4000, 96, &[]), 12),
            ([&old[2048..], &old[..2048]].concat(), 2080),
        ] {
            let patch = compute_patch(&old, &new).unwrap();
            assert!(patch.len() <= max_len, "{} > {max_len}", patch.len());
            assert_eq!(apply_patch(&old, &patch), new);
        }

        let random: Vec<u8> = (0..4096).map(|_| rng.random()).collect();
        assert!(compute_patch(&old, &random).is_none());
    }
}
