use blob_share::packing::{pack_items_brute_force, pack_items_greedy_sorted};
use eyre::Result;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::ops::Range;
use std::path::Path;
use std::{env, fs};

const SEED: u64 = 0;
const BLOB_MAX_SIZE: usize = 131072;
const ONE_GWEI: u128 = 1000000000;

#[test]
fn test_greedy() -> Result<()> {
    for entry in fs::read_dir("tests/packing_test_vectors")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            let mut file = fs::File::open(path)?;
            let mut contents = String::new();
            file.read_to_string(&mut contents)?;
            let mut test_vector: TestVector = serde_json::from_str(&contents)?;

            if let Ok(name) = env::var("TEST_ONLY") {
                if !test_vector.name.contains(&name) {
                    continue;
                }
            }

            if env::var("DUMP_TEST_VECTORS").is_ok() {
                println!("{}: {:?}", test_vector.name, test_vector,);
            }
            if env::var("DUMP_SUMMARY").is_ok() {
                println!(
                    "{}: n {} lens [{},{}] cost_per_len [{},{}], bruteforce solution {}",
                    test_vector.name,
                    test_vector.items.len(),
                    test_vector.items.iter().map(|e| e.0).min().unwrap(),
                    test_vector.items.iter().map(|e| e.0).max().unwrap(),
                    test_vector.items.iter().map(|e| e.1).min().unwrap(),
                    test_vector.items.iter().map(|e| e.1).max().unwrap(),
                    test_vector
                        .brute_force_solution
                        .to_summary_str(&test_vector)
                );
            }

            test_vector.items.sort_by(|a, b| a.0.cmp(&b.0));

            let selected_indexes = pack_items_greedy_sorted(
                test_vector.items.as_mut_slice(),
                test_vector.max_len,
                test_vector.cost_per_len,
            );

            if let Some(selected_indexes) = selected_indexes {
                println!(
                    "{}: {:?}, indexes: {:?}",
                    test_vector.name,
                    SolutionSummary::from_solution(
                        &test_vector.items,
                        &selected_indexes,
                        test_vector.max_len,
                        test_vector.cost_per_len
                    ),
                    selected_indexes,
                );
            } else {
                println!("{}: {:?}", test_vector.name, selected_indexes);
            }
        }
    }

    Ok(())
}

#[test]
fn generate_test_vectors() {
    let mut test_vectors_rand: Vec<TestVectorDef> = vec![];

    for n in [10, 100, 1000] {
        test_vectors_rand.push(TestVectorDef {
            name: format!("exact_price_{n}_"),
            items: ItemsType::RandWithFillerElem((1..BLOB_MAX_SIZE, ONE_GWEI..ONE_GWEI + 1, n)),
            cost_per_len: ONE_GWEI,
            max_len: BLOB_MAX_SIZE,
        });
        for price_mult in [1.1, 1.2, 1.5, 2.0] {
            let upper_p = ((price_mult * 100.0) as u128 * ONE_GWEI) / 100;
            test_vectors_rand.push(TestVectorDef {
                name: format!("price_range_{price_mult}x_{n}_"),
                items: ItemsType::Rand((1..BLOB_MAX_SIZE, ONE_GWEI..upper_p, n)),
                cost_per_len: ONE_GWEI,
                max_len: BLOB_MAX_SIZE,
            });
        }
    }

    for test_vector in test_vectors_rand {
        let file_path = format!("tests/packing_test_vectors/{}.json", test_vector.name);
        if !Path::new(&file_path).exists() {
            let items = test_vector.items.generate(test_vector.max_len);

            let brute_force_solution = if items.len() < 20 {
                match pack_items_brute_force(&items, test_vector.max_len, test_vector.cost_per_len)
                {
                    Some(indexes) => SolutionResult::Ok(indexes),
                    None => SolutionResult::NoSolution,
                }
            } else {
                SolutionResult::NotComputed
            };

            let test_vector = TestVector {
                name: test_vector.name,
                items,
                cost_per_len: test_vector.cost_per_len,
                max_len: test_vector.max_len,
                brute_force_solution,
            };

            let contents = serde_json::to_string(&test_vector).unwrap();
            fs::write(&file_path, contents).expect("Unable to write file");
        }
    }
}

struct TestVectorDef {
    name: String,
    items: ItemsType,
    cost_per_len: u128,
    max_len: usize,
}

type ItemRanges = (Range<usize>, Range<u128>, usize);

enum ItemsType {
    Rand(ItemRanges),
    RandWithFillerElem(ItemRanges),
}

impl ItemsType {
    pub fn generate(&self, max_len: usize) -> Vec<(usize, u128)> {
        match self {
            ItemsType::Rand(ranges) => ItemsType::generate_from_ranges(ranges),

            ItemsType::RandWithFillerElem(ranges) => {
                let mut items = ItemsType::generate_from_ranges(ranges);
                if items.len() < 1 {
                    panic!("RandWithFillerElem requires len > 1");
                }

                // count closest len to max_len
                let mut len = 0;
                for item in items.iter() {
                    if len + item.0 > max_len {
                        break;
                    }
                    len += item.0;
                }
                // Mutate last element so there's a combination of elements that match max_len exactly
                if len < max_len {
                    let last = items.pop().unwrap();
                    items.push((max_len - len - 1, last.1));
                }
                items
            }
        }
    }

    fn generate_from_ranges((range_len, range_cost_per_len, n): &ItemRanges) -> Vec<(usize, u128)> {
        let mut rng = StdRng::seed_from_u64(SEED);
        (0..*n)
            .map(|_| {
                let rand_usize = rng.gen_range(range_len.clone());
                let rand_u128 = rng.gen_range(range_cost_per_len.clone());
                (rand_usize, rand_u128)
            })
            .collect()
    }
}

type Item = (usize, u128);

#[derive(Debug)]
struct SolutionSummary {
    len_used_frac: f64,
    min_cost_per_len_frac: f64,
    items_count: usize,
}

impl SolutionSummary {
    fn from_solution(
        items: &[Item],
        selected_indexes: &[usize],
        max_len: usize,
        cost_per_len: u128,
    ) -> Self {
        let selected_items = selected_indexes
            .iter()
            .map(|i| items[*i])
            .collect::<Vec<_>>();
        let selected_len = selected_items.iter().map(|e| e.0).sum::<usize>();
        let min_cost_per_len = selected_items.iter().map(|e| e.1).min().unwrap();
        Self {
            items_count: selected_indexes.len(),
            len_used_frac: selected_len as f64 / max_len as f64,
            min_cost_per_len_frac: min_cost_per_len as f64 / cost_per_len as f64,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
enum SolutionResult {
    Ok(Vec<usize>),
    NoSolution,
    NotComputed,
}

impl SolutionResult {
    fn to_summary_str(&self, test_vector: &TestVector) -> String {
        match self {
            SolutionResult::Ok(selected_indexes) => {
                let summary = SolutionSummary::from_solution(
                    &test_vector.items,
                    &selected_indexes,
                    test_vector.max_len,
                    test_vector.cost_per_len,
                );
                format!("Ok({:?})", summary)
            }
            SolutionResult::NoSolution => "NoSolution".to_string(),
            SolutionResult::NotComputed => "NotComputed".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct TestVector {
    name: String,
    items: Vec<Item>,
    cost_per_len: u128,
    max_len: usize,
    brute_force_solution: SolutionResult,
}
