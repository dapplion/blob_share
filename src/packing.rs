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
    len_price: u128,
) -> Option<Vec<usize>> {
    let n = items.len();
    let mut best_combination = None;
    let mut best_total_length = 0;
    let max_len_cost = max_len as u128 * len_price;

    // Iterate over all possible combinations
    for mask in 0..(1 << n) as u32 {
        let mut current_length = 0;
        let mut min_len_price_combination = u128::MAX;

        for i in 0..n {
            if mask & (1 << i) != 0 {
                let (len, max_len_price) = items[i];
                current_length += len;

                // Invalid combination
                if current_length > max_len {
                    continue;
                }

                // Track min len price of the combination
                if max_len_price < min_len_price_combination {
                    min_len_price_combination = max_len_price;
                }
            }
        }

        if current_length > 0 {
            // Check if combination is valid
            let effective_len_price = max_len_cost / current_length as u128;
            let is_valid = min_len_price_combination >= effective_len_price;

            if is_valid && current_length > best_total_length {
                best_total_length = current_length;
                best_combination = Some(mask);
                // Found optimal combination
                if current_length == max_len {
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

fn unwrap_items(indexes: Vec<usize>, items: &[PackedItem]) -> Vec<PackedItem> {
    indexes.iter().map(|i| items[*i]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn test_pack_items_brute_force() {
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
        // Select multiple two items at current max price
        run_test_brute_force(100, 1, Some(&[(50, 1), (50, 1)]), &[]);
        run_test_brute_force(100, 1, Some(&[(25, 2), (25, 2)]), &[]);
        run_test_brute_force(100, 1, Some(&[(25, 8), (25, 8)]), &[]);
        run_test_brute_force(100, 1, Some(&[(25, 2), (25, 2), (25, 1), (25, 1)]), &[]);
        // Ignore underpriced items
        run_test_brute_force(100, 2, None, &[(25, 2), (25, 2), (25, 1), (25, 1)]);
        run_test_brute_force(100, 2, Some(&[(25, 4), (25, 4)]), &[(25, 1), (25, 1)]);
    }

    fn run_test_brute_force(
        max_len: usize,
        len_price: u128,
        expected_best_combination: Option<&[PackedItem]>,
        extra_items: &[PackedItem],
    ) {
        let mut items = vec![];
        if let Some(combination) = expected_best_combination {
            items.extend_from_slice(combination);
        }
        items.extend_from_slice(extra_items);

        let best_combination = pack_items_brute_force(&items, max_len, len_price);

        if best_combination != expected_best_combination.map(|v| (0..v.len()).collect()) {
            panic!(
                "case ({max_len}, {len_price}, {expected_best_combination:?}, {extra_items:?}) wrong best combination:\n\n\t{:?} != {:?}\n",
                best_combination.map(|indexes| unwrap_items(indexes, &items)),
                expected_best_combination.map(|s| s.to_vec()),

            );
        }
    }
}
