#![feature(box_patterns)]
#![feature(let_chains)]

use ast::{
    flatten_guards::flatten_guards,
    local_declarations::LocalDeclarer,
    name_locals::name_locals,
    link_upvalues::link_upvalues,
    simplify_gotos::{hoist_locals_for_gotos, simplify_gotos},
};
use by_address::ByAddress;
use cfg::ssa::{
    self,
    structuring::{structure_conditionals, structure_jumps, structure_method_calls},
};
use ast::FxIndexMap as IndexMap;
use lifter::Lifter;
use parking_lot::Mutex;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    time::Instant,
};
use triomphe::Arc;

use clap::Parser;

use lua51_deserializer::chunk::Chunk;

mod lifter;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

// See luau-lifter: the lifter is allocator-bound, mimalloc's per-thread
// free-lists replace the slow Windows system allocator. Gated off under dhat.
#[cfg(not(feature = "dhat-heap"))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    #[clap(short, long)]
    file: String,
}

fn main() -> anyhow::Result<()> {
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();

    let args = Args::parse();
    let path = Path::new(&args.file);
    let mut input = File::open(path)?;
    let mut buffer = vec![0; input.metadata()?.len() as usize];
    input.read_exact(&mut buffer)?;

    let start = Instant::now();
    let chunk = match Chunk::parse(&buffer) {
        Ok((_, chunk)) => chunk,
        Err(error) => anyhow::bail!(
            "not a supported Lua 5.1 chunk: {:?}",
            error.map(|error| (buffer.len() - error.input.len(), error.code))
        ),
    };
    let res = decompile(&chunk.function, true)?;
    let duration = start.elapsed();

    // TODO: use BufWriter?
    let mut out = File::create(path.with_extension("dec.51.lua").file_name().unwrap())?;
    writeln!(out, "-- decompiled by Sentinel (took {:?})", duration)?;
    writeln!(out, "{}", res)?;

    Ok(())
}

fn decompile(prototype: &lua51_deserializer::Function<'_>, parallel: bool) -> anyhow::Result<String> {
    ast::reset_local_ids();
    let mut lifted = Vec::new();
    let (function, upvalues) = Lifter::lift(prototype, &mut lifted);
    lifted.push((Arc::<Mutex<_>>::default(), function, upvalues));
    lifted.reverse();

    let main = Arc::clone(&lifted.first().unwrap().0);
    let id_base = ast::current_local_id();
    let function_count = lifted.len() as u64;
    const ID_STRIDE: u64 = 1 << 40;
    type Lifted = (Arc<Mutex<ast::Function>>, cfg::function::Function, Vec<ast::RcLocal>);
    let process = |(index, (ast_function, mut function, upvalues_in)): (usize, Lifted)| {
            // Same deterministic disjoint ranges as the Luau pipeline. Rayon
            // workers can execute functions in any order without sharing IDs.
            ast::set_local_id_base(id_base + index as u64 * ID_STRIDE);
            let (local_count, local_groups, upvalue_in_groups, upvalue_passed_groups) =
                cfg::ssa::construct(&mut function, &upvalues_in);
            let upvalue_to_group = upvalue_in_groups
                .into_iter()
                .chain(
                    upvalue_passed_groups
                        .into_iter()
                        .map(|m| (ast::RcLocal::default(), m)),
                )
                .flat_map(|(i, g)| g.into_iter().map(move |u| (u, i.clone())))
                .collect::<IndexMap<_, _>>();
            // TODO: do we even need this?
            let local_to_group = local_groups
                .into_iter()
                .enumerate()
                .flat_map(|(i, g)| g.into_iter().map(move |l| (l, i)))
                .collect::<FxHashMap<_, _>>();
            // TODO: REFACTOR: some way to write a macro that states
            // if cfg::ssa::inline results in change then structure_jumps, structure_compound_conditionals,
            // structure_for_loops and remove_unnecessary_params must run again.
            // if structure_compound_conditionals results in change then dominators and post dominators
            // must be recalculated.
            // etc.
            // the macro could also maybe generate an optimal ordering?
            let mut changed = true;
            let mut dominator_cache = None;
            while changed {
                changed = false;

                let dominators = dominator_cache.get_or_insert_with(||
                    cfg::dominators::Dominators::new(function.graph(), function.entry().unwrap()));
                let topology_changed = structure_jumps(&mut function, dominators);
                changed |= topology_changed;

                ssa::inline::inline(&mut function, &local_to_group, &upvalue_to_group);

                let conditionals_changed = structure_conditionals(&mut function, &|local| upvalue_to_group.contains_key(local));
                if topology_changed || conditionals_changed { dominator_cache = None; }
                if conditionals_changed
                // || {
                //     let post_dominators = post_dominators(function.graph_mut());
                //     structure_for_loops(&mut function, &dominators, &post_dominators)
                // }
                    || structure_method_calls(&mut function)
                {
                    changed = true;
                }
                let mut local_map = FxHashMap::default();
                // TODO: loop until returns false?
                if ssa::construct::remove_unnecessary_params(&mut function, &mut local_map, None, None) {
                    changed = true;
                }
                ssa::construct::apply_local_map(&mut function, local_map);
            }
            ssa::Destructor::new(
                &mut function,
                upvalue_to_group,
                upvalues_in.iter().cloned().collect(),
                local_count,
            )
            .destruct();

            let params = function.parameters.clone();
            let is_variadic = function.is_variadic;
            let ignored = upvalues_in.iter().chain(params.iter()).cloned().collect();
            let mut lifted =
                match restructure::lift_source_like_attempt_with_ignored_locals(function, &ignored)
                {
                    restructure::StructureAttempt::Structured(block) => block,
                    rejection => anyhow::bail!(
                        "no proven source representation for Lua 5.1 function: {rejection:?}"
                    ),
                };
            simplify_gotos(&mut lifted);
            flatten_guards(&mut lifted);
            let block = Arc::new(lifted.into());
            LocalDeclarer::default().declare_locals(
                // TODO: why does block.clone() not work?
                Arc::clone(&block),
                &upvalues_in.iter().chain(params.iter()).cloned().collect(),
            );
            hoist_locals_for_gotos(&mut block.lock());

            {
                let mut ast_function = ast_function.lock();
                ast_function.body = Arc::try_unwrap(block).unwrap().into_inner();
                ast_function.parameters = params;
                ast_function.is_variadic = is_variadic;
            }
            Ok((ByAddress(ast_function), upvalues_in))
        };
    // Collect in input order before resolving errors, so the first failed
    // function is deterministic as well as every successful function's IDs.
    let results = if parallel && lifted.len() > 1 {
        lifted.into_par_iter().enumerate().map(process).collect::<Vec<_>>()
    } else {
        lifted.into_iter().enumerate().map(process).collect::<Vec<_>>()
    };
    ast::set_local_id_base(id_base + function_count * ID_STRIDE);
    let mut upvalues = results.into_iter().collect::<anyhow::Result<FxHashMap<_, _>>>()?;

    let main = ByAddress(main);
    upvalues.remove(&main);
    let mut body = Arc::try_unwrap(main.0).unwrap().into_inner().body;
    link_upvalues(&mut body, &mut upvalues);
    name_locals(&mut body, true);
    Ok(body.to_string())
}

#[cfg(test)]
mod scheduling_tests {
    use super::*;

    fn prototype(words: Vec<u32>, children: Vec<Vec<u8>>, upvalues: u8, stack: u8) -> Vec<u8> {
        let mut bytes = vec![0; 12];
        bytes.extend([upvalues, 0, 0, stack]);
        bytes.extend((words.len() as u32).to_le_bytes());
        bytes.extend(words.into_iter().flat_map(u32::to_le_bytes));
        bytes.extend(1u32.to_le_bytes());
        bytes.push(3);
        bytes.extend(7f64.to_le_bytes());
        bytes.extend((children.len() as u32).to_le_bytes());
        for child in children { bytes.extend(child); }
        bytes.extend([0; 12]); // line positions, debug locals and upvalue names
        bytes
    }

    fn captured_child(depth: usize) -> Vec<u8> {
        if depth == 0 {
            prototype(vec![4, 30 | (2 << 23)], vec![], 1, 1)
        } else {
            // CLOSURE r0 child0; capture parent's UPVAL0; RETURN r0.
            prototype(vec![36, 4, 30 | (2 << 23)], vec![captured_child(depth - 1)], 1, 1)
        }
    }

    #[test]
    fn closure_capture_source_is_identical_in_serial_and_one_or_four_threads() {
        for depth in [0, 2, 6] {
            let mut words = vec![1]; // LOADK r0 7
            let mut children = Vec::new();
            for index in 0..5 {
                words.extend([36 | ((index + 1) << 6) | (index << 14), 0]); // capture r0
                children.push(captured_child(depth));
            }
            words.push(30 | (1 << 6) | (6 << 23)); // return the five closures
            let bytes = prototype(words, children, 0, 6);
            let (rest, prototype) = lua51_deserializer::Function::parse(&bytes).unwrap();
            assert!(rest.is_empty());
            let serial = decompile(&prototype, false).unwrap();
            assert!(serial.contains("function"));
            for threads in [1, 4] {
                let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
                for _ in 0..3 {
                    assert_eq!(pool.install(|| decompile(&prototype, true)).unwrap(), serial);
                }
            }
        }
    }
}
