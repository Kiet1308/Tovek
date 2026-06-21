#![feature(box_patterns)]
#![feature(let_chains)]

use ast::{
    flatten_guards::flatten_guards,
    local_declarations::LocalDeclarer,
    name_locals::name_locals,
    replace_locals::replace_locals,
    simplify_gotos::{hoist_locals_for_gotos, simplify_gotos},
    Traverse,
};
use by_address::ByAddress;
use cfg::ssa::{
    self,
    structuring::{structure_conditionals, structure_jumps, structure_method_calls},
};
use indexmap::IndexMap;
use lifter::Lifter;
use parking_lot::Mutex;
use petgraph::algo::dominators::simple_fast;
use rayon::iter::ParallelIterator;
use rayon::prelude::IntoParallelIterator;
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
    let chunk = Chunk::parse(&buffer).unwrap().1;
    let mut lifted = Vec::new();
    let (function, upvalues) = Lifter::lift(&chunk.function, &mut lifted);
    lifted.push((Arc::<Mutex<_>>::default(), function, upvalues));
    lifted.reverse();

    let (main, ..) = lifted.first().unwrap().clone();
    let mut upvalues = lifted
        .into_iter()
        .map(|(ast_function, mut function, upvalues_in)| {
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
            while changed {
                changed = false;

                let dominators = simple_fast(function.graph(), function.entry().unwrap());
                changed |= structure_jumps(&mut function, &dominators);

                ssa::inline::inline(&mut function, &local_to_group, &upvalue_to_group);

                if structure_conditionals(&mut function)
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
                if ssa::construct::remove_unnecessary_params(&mut function, &mut local_map, None) {
                    changed = true;
                }
                ssa::construct::apply_local_map(&mut function, local_map);
            }
            ssa::Destructor::new(
                &mut function,
                upvalue_to_group,
                upvalues_in.iter().cloned().collect(),
                local_count,
                local_to_group,
            )
            .destruct();

            let params = std::mem::take(&mut function.parameters);
            let is_variadic = function.is_variadic;
            let mut lifted = restructure::lift(function);
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
            (ByAddress(ast_function), upvalues_in)
        })
        .collect::<FxHashMap<_, _>>();

    let main = ByAddress(main);
    upvalues.remove(&main);
    let mut body = Arc::try_unwrap(main.0).unwrap().into_inner().body;
    link_upvalues(&mut body, &mut upvalues);
    name_locals(&mut body, true);
    let res = body.to_string();
    let duration = start.elapsed();

    // TODO: use BufWriter?
    let mut out = File::create(path.with_extension("dec.51.lua").file_name().unwrap())?;
    writeln!(out, "-- decompiled by Sentinel (took {:?})", duration)?;
    writeln!(out, "{}", res)?;

    Ok(())
}

fn link_upvalues(
    body: &mut ast::Block,
    upvalues: &mut FxHashMap<ByAddress<Arc<Mutex<ast::Function>>>, Vec<ast::RcLocal>>,
) {
    for stat in &mut body.0 {
        stat.traverse_rvalues(&mut |rvalue| {
            if let ast::RValue::Closure(closure) = rvalue {
                let old_upvalues = upvalues.remove(&closure.function).unwrap();
                let mut function = closure.function.lock();
                // TODO: inefficient, try constructing a map of all up -> new up first
                // and then call replace_locals on main body
                let mut local_map =
                    FxHashMap::with_capacity_and_hasher(old_upvalues.len(), Default::default());
                for (old, new) in
                    old_upvalues
                        .iter()
                        .zip(closure.upvalues.iter().map(|u| match u {
                            ast::Upvalue::Copy(l) | ast::Upvalue::Ref(l) => l,
                        }))
                {
                    // println!("{} -> {}", old, new);
                    local_map.insert(old.clone(), new.clone());
                }
                link_upvalues(&mut function.body, upvalues);
                replace_locals(&mut function.body, &local_map);
            }
        });
        match stat {
            ast::Statement::If(r#if) => {
                link_upvalues(&mut r#if.then_block.lock(), upvalues);
                link_upvalues(&mut r#if.else_block.lock(), upvalues);
            }
            ast::Statement::While(r#while) => {
                link_upvalues(&mut r#while.block.lock(), upvalues);
            }
            ast::Statement::Repeat(repeat) => {
                link_upvalues(&mut repeat.block.lock(), upvalues);
            }
            ast::Statement::NumericFor(numeric_for) => {
                link_upvalues(&mut numeric_for.block.lock(), upvalues);
            }
            ast::Statement::GenericFor(generic_for) => {
                link_upvalues(&mut generic_for.block.lock(), upvalues);
            }
            _ => {}
        }
    }
}
