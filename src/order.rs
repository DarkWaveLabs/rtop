//! A stable sort with one copy of the sort code for every element type.
//!
//! `slice::sort_by` makes a new copy of the sort code (about 6 KiB in the
//! release build) for each element type and closure. rtop sorts five types,
//! and those copies alone put the binary over the size budget (design §4).

use std::cmp::Ordering;

/// Sorts `items` stably by `cmp`, the same as `items.sort_by(cmp)`.
pub fn sort_by<T>(items: &mut [T], cmp: impl Fn(&T, &T) -> Ordering) {
    let mut order: Vec<usize> = (0..items.len()).collect();
    sort_indices(&mut order, &|a, b| cmp(&items[a], &items[b]));
    permute(items, &order);
}

/// The only copy of the sort code: a stable sort of indices.
#[inline(never)]
fn sort_indices(order: &mut [usize], cmp: &dyn Fn(usize, usize) -> Ordering) {
    order.sort_by(|&a, &b| cmp(a, b));
}

/// Moves `items[order[i]]` to position `i`. `order` is a permutation of `0..items.len()`.
fn permute<T>(items: &mut [T], order: &[usize]) {
    // target[old] is the new position of the item now at `old`.
    let mut target = vec![0; order.len()];
    for (new, &old) in order.iter().enumerate() {
        target[old] = new;
    }
    for i in 0..items.len() {
        // Each swap puts one item at its final place, so the loop ends.
        while target[i] != i {
            let j = target[i];
            items.swap(i, j);
            target.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_like_std_sort_by() {
        let mut ours = vec![5, 3, 9, 1, 1, 7, 0, 3, 8, 2, 6, 4];
        let mut std = ours.clone();
        sort_by(&mut ours, |a, b| a.cmp(b));
        std.sort();
        assert_eq!(ours, std);
    }

    #[test]
    fn keeps_equal_items_in_their_order() {
        let mut items = vec![(2, 'a'), (1, 'b'), (2, 'c'), (1, 'd'), (0, 'e'), (2, 'f')];
        sort_by(&mut items, |a, b| a.0.cmp(&b.0));
        assert_eq!(
            items,
            vec![(0, 'e'), (1, 'b'), (1, 'd'), (2, 'a'), (2, 'c'), (2, 'f')]
        );
    }

    #[test]
    fn sorts_descending_and_moves_owned_values() {
        let mut items: Vec<String> = ["b", "d", "a", "c"].map(String::from).to_vec();
        sort_by(&mut items, |a, b| b.cmp(a));
        assert_eq!(items, ["d", "c", "b", "a"]);
    }

    #[test]
    fn handles_empty_and_single() {
        let mut empty: Vec<u8> = Vec::new();
        sort_by(&mut empty, |a, b| a.cmp(b));
        assert!(empty.is_empty());
        let mut one = vec![42];
        sort_by(&mut one, |a, b| a.cmp(b));
        assert_eq!(one, [42]);
    }

    #[test]
    fn matches_std_on_many_permutations() {
        // A small LCG gives repeatable pseudo-random input with many ties.
        let mut seed: u32 = 1;
        for len in 0..40 {
            let mut items: Vec<(u8, usize)> = (0..len)
                .map(|i| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    ((seed >> 16) as u8 % 5, i)
                })
                .collect();
            let mut std = items.clone();
            std.sort_by_key(|a| a.0);
            sort_by(&mut items, |a, b| a.0.cmp(&b.0));
            assert_eq!(items, std, "len {len}");
        }
    }
}
