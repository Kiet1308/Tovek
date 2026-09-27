//! Focused old-schedule/current-algorithm probes. Example:
//! `benchmark_pipeline link 256 7` or `benchmark_pipeline metadata 4000 7`.
//! Fixture creation and semantic checks are outside timed intervals. Metadata
//! timing includes constructing the new per-prototype index.
#[path = "../src/metadata_index.rs"]
mod metadata_index;

use ast::{Block, Closure, Function, LocalRw, RValue, RcLocal, Return, Traverse, Upvalue};
use by_address::ByAddress;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::time::Instant;
use triomphe::Arc;

#[cfg(feature = "allocation-counts")]
#[path = "support/allocation_counts.rs"]
mod allocation_counts;
#[cfg(feature = "allocation-counts")]
#[global_allocator]
static ALLOC: allocation_counts::Counting<mimalloc::MiMalloc> = allocation_counts::Counting(mimalloc::MiMalloc);
#[cfg(not(feature = "allocation-counts"))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

type Inputs = FxHashMap<ByAddress<Arc<Mutex<Function>>>, Vec<RcLocal>>;

fn chain(size: usize) -> (Block, Inputs, u64) {
    ast::reset_local_ids();
    let locals = (0..=size).map(|_| RcLocal::default()).collect::<Vec<_>>();
    let mut body: Block = vec![Return::new(vec![locals[size].clone().into()]).into()].into();
    let mut inputs = Inputs::default();
    for depth in (0..size).rev() {
        let function = ByAddress(Arc::new(Mutex::new(Function { body, ..Default::default() })));
        inputs.insert(function.clone(), vec![locals[depth + 1].clone()]);
        body = vec![Return::new(vec![locals[depth].clone().into(), Closure {
            node_origin: Default::default(), function,
            upvalues: vec![Upvalue::Ref(locals[depth].clone())],
        }.into()]).into()].into();
    }
    (body, inputs, locals[0].stable_id())
}

// Old schedule restricted to this probe's return/closure-only fixture. Every
// descendant is revisited by replace_locals for every enclosing closure.
fn old_link(body: &mut Block, inputs: &Inputs) {
    for statement in &mut body.0 {
        statement.traverse_rvalues(&mut |value| {
            if let RValue::Closure(closure) = value {
                let map = inputs[&closure.function].iter().zip(&closure.upvalues)
                    .map(|(old, capture)| {
                        let (Upvalue::Copy(new) | Upvalue::Ref(new)) = capture;
                        (old.clone(), new.clone())
                    }).collect::<FxHashMap<_, _>>();
                let mut function = closure.function.lock();
                old_link(&mut function.body, inputs);
                ast::replace_locals::replace_locals(&mut function.body, &map);
            }
        });
    }
}

fn check_link(body: &mut Block, root: u64) -> usize {
    let mut count = 0;
    for statement in &mut body.0 {
        for local in statement.values_read() {
            assert_eq!(local.stable_id(), root);
            count += 1;
        }
        statement.traverse_rvalues(&mut |value| {
            if let RValue::Closure(closure) = value {
                count += check_link(&mut closure.function.lock().body, root);
            }
        });
    }
    count
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let mode = &args[1];
    let size = args[2].parse::<usize>().unwrap();
    let repeats = args.get(3).map_or(7, |n| n.parse().unwrap());
    assert!(size > 0 && repeats > 0);
    let ranges = (0..size).map(|i| (0u8, i * 2, i * 2 + 2)).collect::<Vec<_>>();
    let mut rows = Vec::new();
    // Alternate old/new order each round to avoid systematically favoring one.
    for round in 0..repeats {
        for current in if round % 2 == 0 { [false, true] } else { [true, false] } {
            let mut fixture = (mode == "link").then(|| chain(size));
            #[cfg(feature = "allocation-counts")]
            let allocations = allocation_counts::begin();
            let start = Instant::now();
            let checksum;
            if let Some((body, inputs, _)) = &mut fixture {
                if current { ast::link_upvalues::link_upvalues(body, inputs); }
                else { old_link(body, inputs); }
                checksum = 0;
            } else {
                assert_eq!(mode, "metadata");
                let mut sum = 0usize;
                if current {
                    let index = metadata_index::RegisterRanges::new(ranges.iter().copied());
                    let mut found = Vec::new();
                    for pc in 0..size * 2 {
                        found.clear();
                        index.covering(0, pc, &mut found);
                        sum += found.iter().sum::<usize>();
                    }
                } else {
                    for pc in 0..size * 2 {
                        for (ordinal, &(register, start, end)) in ranges.iter().enumerate() {
                            if register == 0 && start <= pc && pc < end { sum += ordinal; }
                        }
                    }
                }
                checksum = std::hint::black_box(sum);
            }
            let ns = start.elapsed().as_nanos();
            #[cfg(feature = "allocation-counts")]
            let allocations = allocation_counts::finish(allocations);
            if let Some((body, _, root)) = &mut fixture {
                assert_eq!(check_link(body, *root), size * 2 + 1);
            } else { assert_eq!(checksum, size * (size - 1)); }
            let mut row = serde_json::json!({"round":round,"current":current,"ns":ns,"checksum":checksum});
            #[cfg(feature = "allocation-counts")]
            { row["allocations"] = allocations; }
            rows.push(row.take());
        }
    }
    println!("{}", serde_json::json!({"schema_version":1,"mode":mode,"size":size,"repeats":repeats,
        "instrumented":cfg!(feature="allocation-counts"),"rows":rows}));
}
