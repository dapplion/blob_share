use std::cmp;

use crate::{data_intent::BlobGasPrice, increase_by_min_percent};

/// (len, max_len_price)
pub type Item = (usize, BlobGasPrice);

const MAX_COUNT_FOR_BRUTEFORCE: usize = 8;

pub fn pack_items(
    items: &[Item],
    max_len: usize,
    cost_per_len: BlobGasPrice,
) -> Option<Vec<usize>> {
    if items.len() < MAX_COUNT_FOR_BRUTEFORCE {
        return pack_items_brute_force(items, max_len, cost_per_len);
    }

    // Filter items that don't event meet the current len price
    let mut items = items
        .iter()
        .filter(|(_, max_len_price)| *max_len_price >= cost_per_len)
        .copied()
        .enumerate()
        .collect::<Vec<(usize, Item)>>();

    // let items_len_sum = items.iter().map(|(len, _)| len).sum::<usize>();
    // if items_len_sum < max_len {
    // special case
    //  }

    items.sort_by(|a, b| a.1 .0.cmp(&b.1 .0));

    let index_ordered = items.iter().map(|e| e.0).collect::<Vec<_>>();
    let items_sorted = items.into_iter().map(|e| e.1).collect::<Vec<_>>();

    // TODO: consider other algos
    pack_items_greedy_sorted(&items_sorted, max_len, cost_per_len).map(|selected_indexes_sorted| {
        selected_indexes_sorted
            .iter()
            .map(|i| index_ordered[*i])
            .collect::<Vec<_>>()
    })
}

/// Returns the combination of items with sum of len closest to `max_len` where all items satisfy
/// the condition `effective_cost_per_len <= item.max_len_price`
///
/// # Panics
///
/// `items.len()` must be < 32
///
/// # Performance
///
/// Computational complexity of this function is $O(n2^n)$ where `n = items.len()`. Should only be
/// used for <= 16 items.
pub fn pack_items_brute_force(
    items: &[Item],
    max_len: usize,
    cost_per_len: BlobGasPrice,
) -> Option<Vec<usize>> {
    let n = items.len();
    // Max n to shift mask to
    assert!(n < 32);

    let mut best_combination = None;
    let mut best_selected_len = 0;
    let fixed_cost = max_len as u128 * cost_per_len as u128;

    // Iterate over all possible combinations
    'comb: for mask in 0..(1_u32 << n) {
        let mut selected_len = 0;
        let mut min_len_price_combination = BlobGasPrice::MAX;

        for (i, item) in items.iter().enumerate().take(n) {
            if mask & (1 << i) != 0 {
                let (len, max_len_price) = item;
                selected_len += len;

                // Invalid combination, stop early
                if selected_len > max_len {
                    continue 'comb;
                }

                // Track min len price of the combination
                if *max_len_price < min_len_price_combination {
                    min_len_price_combination = *max_len_price;
                }
            }
        }

        if selected_len > 0 {
            // Check if combination is valid
            if item_is_priced_ok(fixed_cost, selected_len, min_len_price_combination)
                && selected_len > best_selected_len
            {
                best_selected_len = selected_len;
                best_combination = Some(mask);
                // Found optimal combination
                if selected_len == max_len {
                    break;
                }
            }
        }
    }

    if let Some(mask) = best_combination {
        let mut best_items = vec![];
        for i in 0..n {
            if mask & (1 << i) != 0 {
                best_items.push(i);
            }
        }
        Some(best_items)
    } else {
        None
    }
}

fn item_is_priced_ok(fixed_cost: u128, selected_len: usize, max_len_price: BlobGasPrice) -> bool {
    fixed_cost / (selected_len as u128) <= max_len_price as u128
}

pub fn pack_items_knapsack(
    items: &[Item],
    max_len: usize,
    _cost_per_len: BlobGasPrice,
) -> Option<Vec<usize>> {
    // TODO: consider max_cost
    let item_lens = items.iter().map(|e| e.0).collect::<Vec<_>>();
    Some(knapsack(max_len, &item_lens, &item_lens))
}

/// Ref: Space optimized Approach for 0/1 Knapsack Problem using Dynamic Programming:
/// <https://www.geeksforgeeks.org/0-1-knapsack-problem-dp-10>
fn knapsack(w_max: usize, wt: &[usize], val: &[usize]) -> Vec<usize> {
    assert_eq!(wt.len(), val.len());
    let n = wt.len();

    let mut dp = vec![0; w_max + 1];
    let mut sel: Vec<Vec<usize>> = vec![vec![]; w_max + 1];

    for i in 0..n {
        for w in (0..=w_max).rev() {
            if wt[i] <= w {
                // If the current item's weight is less than the weight ptr
                let dp_adding = dp[w - wt[i]] + val[i];
                if dp_adding > dp[w] {
                    dp[w] = dp_adding;

                    sel[w] = sel[w - wt[i]].clone();
                    sel[w].push(i);
                }
            }
        }
    }

    sel[w_max].clone()
}

/// Expects items to by sorted ascending by data len
pub fn pack_items_greedy_sorted(
    items: &[Item],
    max_len: usize,
    cost_per_len: BlobGasPrice,
) -> Option<Vec<usize>> {
    // Keep only items that price at least the current cost

    let mut min_cost_per_len_to_select = cost_per_len;
    loop {
        match pick_first_items_sorted_ascending(
            items,
            max_len,
            cost_per_len,
            min_cost_per_len_to_select,
        ) {
            PickResult::Some(indexes) => return Some(indexes),
            PickResult::InvalidSelection => {
                if min_cost_per_len_to_select > cost_per_len * 2 {
                    return None;
                } else {
                    // Handles low values to at ensure that min_cost increases in each loop
                    min_cost_per_len_to_select =
                        increase_by_min_percent(min_cost_per_len_to_select, 110);
                }
            }
            PickResult::EmptySelection => return None,
        }
    }
}

enum PickResult {
    Some(Vec<usize>),
    InvalidSelection,
    EmptySelection,
}

fn pick_first_items_sorted_ascending(
    items: &[Item],
    max_len: usize,
    cost_per_len: BlobGasPrice,
    min_cost_per_len_to_select: BlobGasPrice,
) -> PickResult {
    let mut len = 0;
    let mut min_max_price = BlobGasPrice::MAX;
    let mut indexes = vec![];
    for (i, item) in items.iter().enumerate() {
        if item.1 >= min_cost_per_len_to_select {
            // Ascending sort, any next item will be over the limit
            if len + item.0 > max_len {
                break;
            }
            len += item.0;
            min_max_price = cmp::min(min_max_price, item.1);
            indexes.push(i);
        }
    }

    // check if min_max_price is satisfied
    // effective_cost_per_len = max_len * cost_per_len / len < min_max_price
    if len == 0 {
        PickResult::EmptySelection
    } else if (max_len as u128 * cost_per_len as u128) <= len as u128 * min_max_price as u128 {
        PickResult::Some(indexes)
    } else {
        PickResult::InvalidSelection
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn test_pack_items_brute_force_manual() {
        // Empty case
        run_test_brute_force(100, 1, None, &[]);
        // Select single item that fills all space
        run_test_brute_force(100, 1, Some(&[(100, 1)]), &[]);
        // Don't select item that is under priced
        run_test_brute_force(100, 2, None, &[(100, 1)]);
        // Don't select item that does not fill the entire blob
        run_test_brute_force(100, 1, None, &[(50, 1)]);
        // Select items that can pay for extra premium space
        run_test_brute_force(100, 1, Some(&[(50, 2)]), &[]);
        // Ignore items exceeding length
        run_test_brute_force(100, 1, Some(&[(100, 1)]), &[(100, 1)]);
        run_test_brute_force(100, 1, Some(&[(99, 2)]), &[(2, 2)]);
        // Select multiple two items at current max price
        run_test_brute_force(100, 1, Some(&[(50, 1), (50, 1)]), &[]);
        run_test_brute_force(100, 1, Some(&[(25, 2), (25, 2)]), &[]);
        run_test_brute_force(100, 1, Some(&[(25, 8), (25, 8)]), &[]);
        run_test_brute_force(100, 1, Some(&[(25, 2), (25, 2), (25, 1), (25, 1)]), &[]);
        // Ignore underpriced items
        run_test_brute_force(100, 2, None, &[(25, 2), (25, 2), (25, 1), (25, 1)]);
        run_test_brute_force(100, 2, Some(&[(25, 4), (25, 4)]), &[(25, 1), (25, 1)]);
        // Test actual numbers
        run_test_brute_force(
            131072,
            30_000_000_000,
            Some(&[(65521, 31_000_000_000), (65523, 31_000_000_000)]),
            &[(32768, 30_200_000_000), (16384, 30_500_000_000)],
        );
        // proptest cases
        run_test_brute_force(32, 3, Some(&[(3, 94)]), &[]);
        run_test_brute_force(32, 9, Some(&[(8, 9), (23, 9)]), &[(33, 9)]);
    }

    fn run_test_brute_force(
        max_len: usize,
        cost_per_len: BlobGasPrice,
        expected_best_combination: Option<&[Item]>,
        extra_items: &[Item],
    ) {
        let mut items = vec![];
        if let Some(combination) = expected_best_combination {
            items.extend_from_slice(combination);
        }
        items.extend_from_slice(extra_items);

        let best_combination = pack_items_brute_force(&items, max_len, cost_per_len);

        if best_combination != expected_best_combination.map(|v| (0..v.len()).collect()) {
            panic!(
                "case ({max_len}, {cost_per_len}, {expected_best_combination:?}, {extra_items:?}) wrong best combination:\n\n\t{:?} != {:?}\n",
                best_combination.map(|indexes| unwrap_items(indexes, &items)),
                expected_best_combination.map(|s| s.to_vec()),

            );
        }
    }

    proptest! {
        #[test]
        fn test_pack_items_brute_force_proptest(
            items in prop::collection::vec((0..50usize, 1..1000 as BlobGasPrice), 1..10), // Generate vectors of items (length, max_price)
            max_len in 1..100usize, // Random max length
            cost_per_len in 1..10 as BlobGasPrice, // Random price per length unit
        ) {
        if let Some(indexes) = pack_items_brute_force(&items, max_len, cost_per_len) {
            let selected_items = unwrap_items(indexes, &items);
            let selected_len = items_total_len(&selected_items);
            prop_assert!(selected_len <= max_len);
            for item in selected_items {
                prop_assert!(is_priced_ok(&item, max_len, cost_per_len, selected_len));
            }
        }
        }
    }

    fn is_priced_ok(
        item: &Item,
        max_len: usize,
        cost_per_len: BlobGasPrice,
        selected_len: usize,
    ) -> bool {
        let effective_cost_per_len =
            (max_len as u128 * cost_per_len as u128) / selected_len as u128;
        effective_cost_per_len as BlobGasPrice <= item.1
    }

    fn items_total_len(items: &[Item]) -> usize {
        items.iter().map(|e| e.0).sum()
    }

    proptest! {
        #[test]
        fn test_knapsack_proptest_max_len(
            item_lens in prop::collection::vec(1..1000usize, 1..1000),
            max_len in 1..1000usize, // Random max length
        ) {
            let selected_len = run_test_knapsack_proptest_max_len(&item_lens, max_len);
            prop_assert!(selected_len <= max_len);
        }
    }

    fn run_test_knapsack_proptest_max_len(item_lens: &[usize], max_len: usize) -> usize {
        // score = length of each item
        let values = item_lens;
        let selected_indexes = knapsack(max_len, item_lens, values);
        selected_indexes.iter().map(|i| item_lens[*i]).sum()
    }

    proptest! {
        #[test]
        fn test_knapsack_equals_bruteforce(
            item_lens in prop::collection::vec(1..1000usize, 1..20),
            max_len in 1..1000usize, // Random max length
        ) {
            prop_assert!(run_test_knapsack_equals_bruteforce(&item_lens, max_len));
        }
    }

    fn run_test_knapsack_equals_bruteforce(item_lens: &[usize], max_len: usize) -> bool {
        let items = item_lens
            .iter()
            .map(|len| (*len, 10 * max_len as BlobGasPrice))
            .collect::<Vec<_>>();

        let selected_indexes_knapsack = pack_items_knapsack(&items, max_len, 1).unwrap();

        let selected_indexes_bruteforce =
            pack_items_brute_force(&items, max_len, 1).unwrap_or(vec![]);

        return selected_indexes_knapsack == selected_indexes_bruteforce;
    }

    fn unwrap_items(indexes: Vec<usize>, items: &[Item]) -> Vec<Item> {
        indexes.iter().map(|i| items[*i]).collect()
    }
}
