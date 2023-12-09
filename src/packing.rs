/// (len, max_len_price)
type PackedItem = (usize, u128);

pub fn pack_items(items: &[PackedItem], len_price: u128, max_len: usize) {
    // Filter items that don't event meet the current len price
    let items = items
        .iter()
        .filter(|(_, max_len_price)| *max_len_price >= len_price)
        .collect::<Vec<_>>();

    let items_len_sum = items.iter().map(|(len, _)| len).sum::<usize>();

    if items_len_sum < max_len {
        // special case
    }
}

/// Find the optimal
pub fn pack_items_brute_force(
    items: &[(usize, u128)],
    max_len: usize,
    cost_per_len: u128,
) -> Option<Vec<usize>> {
    let n = items.len();
    let mut best_combination = None;
    let mut best_selected_len = 0;
    let fixed_cost = max_len as u128 * cost_per_len;

    // Iterate over all possible combinations
    'comb: for mask in 0..(1 << n) as u32 {
        let mut selected_len = 0;
        let mut min_len_price_combination = u128::MAX;

        for i in 0..n {
            if mask & (1 << i) != 0 {
                let (len, max_len_price) = items[i];
                selected_len += len;

                // Invalid combination, stop early
                if selected_len > max_len {
                    continue 'comb;
                }

                // Track min len price of the combination
                if max_len_price < min_len_price_combination {
                    min_len_price_combination = max_len_price;
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

fn item_is_priced_ok(fixed_cost: u128, selected_len: usize, max_len_price: u128) -> bool {
    fixed_cost / (selected_len as u128) <= max_len_price
}

fn unwrap_items(indexes: Vec<usize>, items: &[PackedItem]) -> Vec<PackedItem> {
    indexes.iter().map(|i| items[*i]).collect()
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
        cost_per_len: u128,
        expected_best_combination: Option<&[PackedItem]>,
        extra_items: &[PackedItem],
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
            items in prop::collection::vec((0..50usize, 1..1000u128), 1..10), // Generate vectors of items (length, max_price)
            max_len in 1..100usize, // Random max length
            cost_per_len in 1..10u128, // Random price per length unit
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
        item: &PackedItem,
        max_len: usize,
        cost_per_len: u128,
        selected_len: usize,
    ) -> bool {
        let effective_cost_per_len = (max_len as u128 * cost_per_len) / selected_len as u128;
        effective_cost_per_len <= item.1
    }

    fn items_total_len(items: &[PackedItem]) -> usize {
        items.iter().map(|e| e.0).sum()
    }
}
