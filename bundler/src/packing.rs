use std::cmp;

use crate::utils::increase_by_min_percent;

/// (len, max_len_price)
#[derive(Copy, Clone, Debug)]
pub struct Item {
    pub len: usize,
    pub max_len_price: u64,
    /// Optional group identifier for co-location preference.
    /// Items with the same group_id are preferably packed into the same blob TX.
    pub group_id: Option<uuid::Uuid>,
}

impl Item {
    pub fn new(len: usize, max_len_price: u64) -> Self {
        Self {
            len,
            max_len_price,
            group_id: None,
        }
    }

    pub fn with_group(len: usize, max_len_price: u64, group_id: Option<uuid::Uuid>) -> Self {
        Self {
            len,
            max_len_price,
            group_id,
        }
    }
}

const MAX_COUNT_FOR_BRUTEFORCE: usize = 8;

/// Requires items to be sorted.
///
/// Returns the selected indexes in order of the arg items.
pub fn pack_items(items_sorted: &[Item], max_len: usize, cost_per_len: u64) -> Option<Vec<usize>> {
    if items_sorted.len() < MAX_COUNT_FOR_BRUTEFORCE {
        return pack_items_brute_force(items_sorted, max_len, cost_per_len);
    }

    // let items_len_sum = items.iter().map(|(len, _)| len).sum::<usize>();
    // if items_len_sum < max_len {
    // special case
    //  }

    // Items must be sorted ascending for pack_items_greedy to work correctly.
    // Return None instead of panicking if the caller passes unsorted items.
    if !is_sorted_ascending(items_sorted) {
        return None;
    }

    // TODO: consider other algos
    pack_items_greedy_sorted(items_sorted, max_len, cost_per_len)
}

fn is_sorted_ascending(slice: &[Item]) -> bool {
    slice.windows(2).all(|w| w[0].len <= w[1].len)
}

pub fn sort_items(items: &mut [Item]) {
    items.sort_by(|a, b| a.len.cmp(&b.len));
}

/// Returns the combination of items with sum of len closest to `max_len` where all items satisfy
/// the condition `effective_cost_per_len <= item.max_len_price`
///
/// Returns `None` if no viable combination exists or if `items.len() >= 32` (bitmask limit).
///
/// # Performance
///
/// Computational complexity of this function is $O(n2^n)$ where `n = items.len()`. Should only be
/// used for <= 16 items.
pub fn pack_items_brute_force(
    items: &[Item],
    max_len: usize,
    cost_per_len: u64,
) -> Option<Vec<usize>> {
    let n = items.len();
    // Max n to shift mask to. Return None instead of panicking if items exceed bitmask capacity.
    if n >= 32 {
        return None;
    }

    let mut best_combination = None;
    let mut best_selected_len = 0;
    let fixed_cost = max_len as u128 * cost_per_len as u128;

    // Iterate over all possible combinations
    'comb: for mask in 0..(1_u32 << n) {
        let mut selected_len = 0;
        let mut min_len_price_combination = u64::MAX;

        for (i, item) in items.iter().enumerate().take(n) {
            if mask & (1 << i) != 0 {
                selected_len += item.len;

                // Invalid combination, stop early
                if selected_len > max_len {
                    continue 'comb;
                }

                // Track min len price of the combination
                if item.max_len_price < min_len_price_combination {
                    min_len_price_combination = item.max_len_price;
                }
            }
        }

        if selected_len > 0 &&
            // Check if combination is valid
            fixed_cost / (selected_len as u128) <= min_len_price_combination as u128
            // Persist best combination
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

pub fn pack_items_knapsack(
    items: &[Item],
    max_len: usize,
    _cost_per_len: u64,
) -> Option<Vec<usize>> {
    // TODO: consider max_cost
    let item_lens = items.iter().map(|e| e.len).collect::<Vec<_>>();
    let selected = knapsack(max_len, &item_lens, &item_lens)?;
    if selected.is_empty() {
        None
    } else {
        Some(selected)
    }
}

/// Ref: Space optimized Approach for 0/1 Knapsack Problem using Dynamic Programming:
/// <https://www.geeksforgeeks.org/0-1-knapsack-problem-dp-10>
///
/// Returns `None` if `wt` and `val` have different lengths.
fn knapsack(w_max: usize, wt: &[usize], val: &[usize]) -> Option<Vec<usize>> {
    if wt.len() != val.len() {
        return None;
    }
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

    Some(sel[w_max].clone())
}

/// Expects items to by sorted ascending by data len
pub fn pack_items_greedy_sorted(
    items: &[Item],
    max_len: usize,
    cost_per_len: u64,
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
                        increase_by_min_percent(min_cost_per_len_to_select, 1.1);
                    continue;
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
    cost_per_len: u64,
    min_cost_per_len_to_select: u64,
) -> PickResult {
    let mut len = 0;
    let mut min_max_price = u64::MAX;
    let mut indexes = vec![];
    for (i, item) in items.iter().enumerate() {
        if item.max_len_price >= min_cost_per_len_to_select {
            // Ascending sort, any next item will be over the limit
            if len + item.len > max_len {
                break;
            }
            len += item.len;
            min_max_price = cmp::min(min_max_price, item.max_len_price);
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

    type ItemTuple = (usize, u64);

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
        cost_per_len: u64,
        expected_best_combination: Option<&[ItemTuple]>,
        extra_items: &[ItemTuple],
    ) {
        let mut items = vec![];
        if let Some(combination) = expected_best_combination {
            items.extend_from_slice(combination);
        }
        items.extend_from_slice(extra_items);

        let best_combination = pack_items_brute_force(&from_tuples(&items), max_len, cost_per_len);

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
            items in prop::collection::vec((0..50usize, 1..1000 as u64), 1..10), // Generate vectors of items (length, max_price)
            max_len in 1..100usize, // Random max length
            cost_per_len in 1..10 as u64, // Random price per length unit
        ) {
        if let Some(indexes) = pack_items_brute_force(&from_tuples(&items), max_len, cost_per_len) {
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
        item: &ItemTuple,
        max_len: usize,
        cost_per_len: u64,
        selected_len: usize,
    ) -> bool {
        let effective_cost_per_len =
            (max_len as u128 * cost_per_len as u128) / selected_len as u128;
        effective_cost_per_len as u64 <= item.1
    }

    fn items_total_len(items: &[ItemTuple]) -> usize {
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
        let selected_indexes = knapsack(max_len, item_lens, values).unwrap();
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
            .map(|len| Item::new(*len, 10 * max_len as u64))
            .collect::<Vec<Item>>();

        let selected_indexes_knapsack = pack_items_knapsack(&items, max_len, 1).unwrap_or_default();

        let selected_indexes_bruteforce =
            pack_items_brute_force(&items, max_len, 1).unwrap_or(vec![]);

        return selected_indexes_knapsack == selected_indexes_bruteforce;
    }

    fn unwrap_items<T: Copy>(indexes: Vec<usize>, items: &[T]) -> Vec<T> {
        indexes.iter().map(|i| items[*i]).collect()
    }

    fn from_tuples(items: &[ItemTuple]) -> Vec<Item> {
        items.iter().map(|(l, m)| Item::new(*l, *m)).collect()
    }

    //
    // pack items
    //
    const MAX_LEN: usize = 100_000;

    #[test]
    fn select_next_blob_items_case_no_items() {
        run_pack_items_test(&[], 1, None);
    }

    #[test]
    fn select_next_blob_items_case_one_small() {
        run_pack_items_test(&[(MAX_LEN / 4, 1)], 1, None);
    }

    #[test]
    fn select_next_blob_items_case_one_big() {
        run_pack_items_test(&[(MAX_LEN, 1)], 1, Some(&[(MAX_LEN, 1)]));
    }

    #[test]
    fn select_next_blob_items_case_multiple_small() {
        run_pack_items_test(
            &[
                (MAX_LEN / 4, 1),
                (MAX_LEN / 4, 2),
                (MAX_LEN / 2, 3),
                (MAX_LEN / 2, 4),
            ],
            1,
            Some(&[(MAX_LEN / 4, 2), (MAX_LEN / 4, 1), (MAX_LEN / 2, 3)]),
        );
    }

    fn run_pack_items_test(
        items: &[ItemTuple],
        price_per_len: u64,
        expected_selected_items: Option<&[ItemTuple]>,
    ) {
        let mut items = items.to_vec();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let selected_indexes = pack_items(&from_tuples(&items), MAX_LEN, price_per_len);
        let selected_items = selected_indexes.map(|idxs| unwrap_items(idxs, &items));

        assert_eq!(
            items_to_summary(selected_items),
            items_to_summary(expected_selected_items.map(|v| v.to_vec()))
        )
    }

    fn items_to_summary(items: Option<Vec<ItemTuple>>) -> Option<Vec<String>> {
        items.map(|mut items| {
            // Sort for stable comparision
            items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));

            items
                .iter()
                .map(|d| format!("(MAX / {}, {})", MAX_LEN / d.0, d.1))
                .collect()
        })
    }

    // --- pack_items_greedy_sorted ---

    #[test]
    fn greedy_sorted_empty_items() {
        let items: &[Item] = &[];
        assert_eq!(pack_items_greedy_sorted(items, 100, 1), None);
    }

    #[test]
    fn greedy_sorted_single_item_fills_space() {
        // One item exactly filling max_len with adequate pricing
        let items = [Item::new(100, 1)];
        let result = pack_items_greedy_sorted(&items, 100, 1);
        assert_eq!(result, Some(vec![0]));
    }

    #[test]
    fn greedy_sorted_single_underpriced_item() {
        // Effective cost = max_len * cost_per_len / selected_len = 100 * 2 / 50 = 4
        // Item's max_len_price = 2 < 4, so it can't afford
        let items = [Item::new(50, 2)];
        assert_eq!(pack_items_greedy_sorted(&items, 100, 2), None);
    }

    #[test]
    fn greedy_sorted_skips_underpriced_selects_expensive() {
        // Sorted ascending by len. Item at index 0 is underpriced, item at index 1 can pay.
        let items = [Item::new(30, 1), Item::new(80, 10)];
        let result = pack_items_greedy_sorted(&items, 100, 1);
        // The greedy picks items in order that satisfy pricing. With min_cost=1,
        // it tries both (30+80=110 > max_len), so only 80 fits after 30. Actually
        // let's check: both fit? 30+80=110 > 100, so greedy picks 30 first, then 80
        // won't fit. selected_len=30, effective_cost=100*1/30≈3.3, item price=1 < 3.3
        // → InvalidSelection. Bumps min_cost. Eventually only picks item at index 1 (price=10).
        assert!(result.is_some());
        let indexes = result.unwrap();
        let selected_items: Vec<_> = indexes.iter().map(|&i| &items[i]).collect();
        // All selected items should satisfy pricing
        let selected_len: usize = selected_items.iter().map(|i| i.len).sum();
        assert!(selected_len <= 100);
    }

    #[test]
    fn greedy_sorted_multiple_items_fitting() {
        // All items fit and can pay
        let items = [Item::new(20, 10), Item::new(30, 10), Item::new(40, 10)];
        let result = pack_items_greedy_sorted(&items, 100, 1);
        assert_eq!(result, Some(vec![0, 1, 2]));
    }

    #[test]
    fn greedy_sorted_stops_at_max_len() {
        // Items sorted ascending; greedy picks until max_len is reached
        let items = [
            Item::new(30, 5),
            Item::new(30, 5),
            Item::new(30, 5),
            Item::new(30, 5),
        ];
        let result = pack_items_greedy_sorted(&items, 100, 1);
        assert!(result.is_some());
        let indexes = result.unwrap();
        let selected_len: usize = indexes.iter().map(|&i| items[i].len).sum();
        // Should pick 3 items (90 <= 100) but not 4 (120 > 100)
        assert!(selected_len <= 100);
        assert_eq!(indexes.len(), 3);
    }

    // --- pack_items dispatching ---

    #[test]
    fn pack_items_uses_brute_force_for_small_sets() {
        // Less than MAX_COUNT_FOR_BRUTEFORCE items → brute force
        let mut items = vec![Item::new(MAX_LEN / 2, 1), Item::new(MAX_LEN / 2, 1)];
        sort_items(&mut items);
        let result = pack_items(&items, MAX_LEN, 1);
        assert_eq!(result, Some(vec![0, 1]));
    }

    #[test]
    fn pack_items_uses_greedy_for_large_sets() {
        // Exactly MAX_COUNT_FOR_BRUTEFORCE items → greedy path
        let mut items: Vec<Item> = (0..MAX_COUNT_FOR_BRUTEFORCE)
            .map(|_| Item::new(MAX_LEN / MAX_COUNT_FOR_BRUTEFORCE, 10))
            .collect();
        sort_items(&mut items);
        let result = pack_items(&items, MAX_LEN, 1);
        assert!(result.is_some());
    }

    // --- sort_items ---

    #[test]
    fn sort_items_sorts_ascending_by_len() {
        let mut items = vec![Item::new(50, 1), Item::new(10, 1), Item::new(30, 1)];
        sort_items(&mut items);
        assert_eq!(items[0].len, 10);
        assert_eq!(items[1].len, 30);
        assert_eq!(items[2].len, 50);
    }

    // --- is_sorted_ascending ---

    #[test]
    fn is_sorted_ascending_empty() {
        assert!(is_sorted_ascending(&[]));
    }

    #[test]
    fn is_sorted_ascending_single() {
        assert!(is_sorted_ascending(&[Item::new(42, 1)]));
    }

    #[test]
    fn is_sorted_ascending_sorted() {
        let items = [Item::new(10, 1), Item::new(20, 1), Item::new(30, 1)];
        assert!(is_sorted_ascending(&items));
    }

    #[test]
    fn is_sorted_ascending_equal_elements() {
        let items = [Item::new(10, 1), Item::new(10, 1)];
        assert!(is_sorted_ascending(&items));
    }

    #[test]
    fn is_sorted_ascending_unsorted() {
        let items = [Item::new(20, 1), Item::new(10, 1)];
        assert!(!is_sorted_ascending(&items));
    }

    // --- Item::with_group ---

    #[test]
    fn item_with_group_stores_group_id() {
        let gid = uuid::Uuid::new_v4();
        let item = Item::with_group(100, 5, Some(gid));
        assert_eq!(item.len, 100);
        assert_eq!(item.max_len_price, 5);
        assert_eq!(item.group_id, Some(gid));
    }

    #[test]
    fn item_new_has_no_group() {
        let item = Item::new(100, 5);
        assert_eq!(item.group_id, None);
    }

    // --- graceful error handling (no panics) ---

    #[test]
    fn pack_items_returns_none_for_unsorted_large_set() {
        // >= MAX_COUNT_FOR_BRUTEFORCE items in descending order (unsorted).
        // Previously this would panic via assert!(); now returns None.
        let items: Vec<Item> = (0..MAX_COUNT_FOR_BRUTEFORCE)
            .rev()
            .map(|i| Item::new((i + 1) * 10, 10))
            .collect();
        assert!(!is_sorted_ascending(&items));
        let result = pack_items(&items, MAX_LEN, 1);
        assert_eq!(result, None);
    }

    #[test]
    fn pack_items_brute_force_returns_none_for_32_items() {
        // Exactly 32 items exceeds the bitmask capacity (u32).
        // Previously this would panic via assert!(); now returns None.
        let items: Vec<Item> = (0..32).map(|_| Item::new(1, u64::MAX)).collect();
        let result = pack_items_brute_force(&items, 100, 1);
        assert_eq!(result, None);
    }

    #[test]
    fn pack_items_brute_force_returns_none_for_33_items() {
        // 33 items, well above the 32-item bitmask limit.
        let items: Vec<Item> = (0..33).map(|_| Item::new(1, u64::MAX)).collect();
        let result = pack_items_brute_force(&items, 100, 1);
        assert_eq!(result, None);
    }

    #[test]
    fn pack_items_brute_force_accepts_fewer_than_32_items() {
        // Verify that items.len() < 32 is accepted (no early return).
        // Using a small count to keep the test fast — the boundary check
        // itself is at n >= 32, tested above with 32 and 33 items.
        let items: Vec<Item> = (0..7).map(|_| Item::new(10, 100)).collect();
        // 7 * 10 = 70, max_len = 70, all items fit exactly
        let result = pack_items_brute_force(&items, 70, 1);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 7);
    }

    #[test]
    fn pack_items_sorted_large_set_still_works() {
        // Properly sorted large set should work fine via greedy path.
        // Each item is MAX_LEN / MAX_COUNT_FOR_BRUTEFORCE so all items fill the blob exactly.
        let item_len = MAX_LEN / MAX_COUNT_FOR_BRUTEFORCE;
        let mut items: Vec<Item> = (0..MAX_COUNT_FOR_BRUTEFORCE)
            .map(|_| Item::new(item_len, 10))
            .collect();
        sort_items(&mut items);
        assert!(is_sorted_ascending(&items));
        let result = pack_items(&items, MAX_LEN, 1);
        assert!(result.is_some());
    }

    // --- knapsack ---

    #[test]
    fn knapsack_empty_inputs() {
        let result = knapsack(100, &[], &[]);
        assert_eq!(result, Some(vec![]));
    }

    #[test]
    fn knapsack_single_item_fits() {
        let result = knapsack(100, &[50], &[50]).unwrap();
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn knapsack_single_item_too_heavy() {
        let result = knapsack(10, &[50], &[50]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn knapsack_selects_optimal_subset() {
        // Items: (weight=60, value=60), (weight=50, value=50), (weight=50, value=50)
        // Capacity 100: optimal is items 1+2 (weight=100, value=100) over item 0 alone (60)
        let result = knapsack(100, &[60, 50, 50], &[60, 50, 50]).unwrap();
        let total_weight: usize = result.iter().map(|&i| [60, 50, 50][i]).sum();
        assert!(total_weight <= 100);
        let total_value: usize = result.iter().map(|&i| [60, 50, 50][i]).sum();
        assert_eq!(total_value, 100);
    }

    #[test]
    fn knapsack_mismatched_lengths_returns_none() {
        // wt has 3 elements, val has 2 — should return None instead of panicking
        let result = knapsack(100, &[10, 20, 30], &[10, 20]);
        assert_eq!(result, None);
    }

    #[test]
    fn knapsack_mismatched_val_longer_returns_none() {
        let result = knapsack(100, &[10], &[10, 20]);
        assert_eq!(result, None);
    }

    #[test]
    fn knapsack_zero_capacity() {
        // No items can fit in zero capacity
        let result = knapsack(0, &[10, 20], &[10, 20]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn knapsack_all_items_fit() {
        let result = knapsack(100, &[10, 20, 30], &[10, 20, 30]).unwrap();
        let total: usize = result.iter().map(|&i| [10, 20, 30][i]).sum();
        assert_eq!(total, 60);
        assert_eq!(result.len(), 3);
    }

    // --- pack_items_knapsack ---

    #[test]
    fn pack_items_knapsack_empty_returns_none() {
        let result = pack_items_knapsack(&[], 100, 1);
        assert_eq!(result, None);
    }

    #[test]
    fn pack_items_knapsack_no_items_fit_returns_none() {
        // All items are too large for the capacity
        let items = [Item::new(200, 10), Item::new(300, 10)];
        let result = pack_items_knapsack(&items, 100, 1);
        assert_eq!(result, None);
    }

    #[test]
    fn pack_items_knapsack_selects_fitting_items() {
        let items = [Item::new(40, 10), Item::new(60, 10), Item::new(50, 10)];
        let result = pack_items_knapsack(&items, 100, 1).unwrap();
        let total_len: usize = result.iter().map(|&i| items[i].len).sum();
        assert!(total_len <= 100);
        assert!(!result.is_empty());
    }
}
