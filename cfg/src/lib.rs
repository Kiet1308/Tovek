#![feature(box_patterns)]
#![feature(box_into_inner)]
#![feature(let_chains)]
#![feature(if_let_guard)]
#![feature(iter_order_by)]

pub mod block;
pub mod analysis;
pub mod dominators;
pub mod dot;
pub mod function;
pub mod pattern;
pub mod provenance;
pub mod source_bindings;
pub mod ssa;
