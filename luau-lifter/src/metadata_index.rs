//! Register/PC indexes for compiler metadata. Original ordinals are retained:
//! overlapping ranges must keep the same ambiguity and first-hint semantics.

/// Evidence is usable only when exactly one valid source record applies.
/// Display-name equality does not make two source bindings identical.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetadataMatch<T> { Absent, Unique(T), Ambiguous }

pub(crate) fn unique_match<T>(values: impl IntoIterator<Item = T>) -> MetadataMatch<T> {
    let mut values = values.into_iter();
    match (values.next(), values.next()) {
        (None, _) => MetadataMatch::Absent,
        (Some(value), None) => MetadataMatch::Unique(value),
        _ => MetadataMatch::Ambiguous,
    }
}

/// The same admission rule is used by source reconstruction and its audit.
/// Invalid names/ranges are not promoted into source-binding evidence.
pub(crate) fn debug_binding(
    prototype: usize,
    name_index: usize,
    register: u8,
    lifetime: std::ops::Range<usize>,
    instruction_count: usize,
    max_stack_size: u8,
    strings: &[&[u8]],
) -> Option<ast::SourceBinding> {
    if lifetime.start >= lifetime.end || lifetime.end > instruction_count
        || register >= max_stack_size { return None; }
    let name = std::str::from_utf8(strings.get(name_index.checked_sub(1)?)?).ok()?;
    ast::valid_source_name(name).then(|| ast::SourceBinding {
        origin: ast::BindingOrigin::DebugLocal { prototype, register,
            start_pc: lifetime.start, end_pc: lifetime.end },
        name: name.to_owned(),
    })
}

#[derive(Clone, Copy)]
struct Range {
    start: usize,
    end: usize,
    ordinal: usize,
}

#[derive(Default)]
struct Register {
    ranges: Vec<Range>,
    // Complete binary tree of maximum exclusive ends. It skips expired
    // subtrees even when a long enclosing range overlaps many short ranges.
    max_end: Vec<usize>,
    leaves: usize,
}

#[derive(Default)]
pub(crate) struct RegisterRanges {
    registers: Vec<Register>,
}

impl RegisterRanges {
    pub fn new(ranges: impl IntoIterator<Item = (u8, usize, usize)>) -> Self {
        let mut index = Self::default();
        for (ordinal, (register, start, end)) in ranges.into_iter().enumerate() {
            let register = usize::from(register);
            index.registers.resize_with(index.registers.len().max(register + 1), Register::default);
            index.registers[register].ranges.push(Range { start, end, ordinal });
        }
        for register in &mut index.registers {
            if register.ranges.is_empty() { continue; }
            register.ranges.sort_unstable_by_key(|range| (range.start, range.ordinal));
            register.leaves = register.ranges.len().next_power_of_two();
            register.max_end = vec![0; register.leaves * 2];
            for (i, range) in register.ranges.iter().enumerate() {
                register.max_end[register.leaves + i] = range.end;
            }
            for i in (1..register.leaves).rev() {
                register.max_end[i] = register.max_end[i * 2].max(register.max_end[i * 2 + 1]);
            }
        }
        index
    }

    pub fn register_count(&self) -> usize { self.registers.len() }

    /// Append the source ordinals of all ranges containing `pc`.
    pub fn covering(&self, register: u8, pc: usize, out: &mut Vec<usize>) {
        let Some(register) = self.registers.get(usize::from(register)) else { return; };
        if register.ranges.is_empty() { return; }
        let prefix = register.ranges.partition_point(|range| range.start <= pc);
        register.covering(1, 0, register.leaves, prefix, pc, out);
    }

    /// An initializer at `after` reaches ranges starting no later than the
    /// next write (or block end); starts equal to the write itself are excluded.
    pub fn starting_after_through(&self, register: u8, after: usize, through: usize, out: &mut Vec<usize>) {
        if after >= through { return; }
        let Some(register) = self.registers.get(usize::from(register)) else { return; };
        let begin = register.ranges.partition_point(|range| range.start <= after);
        let end = register.ranges.partition_point(|range| range.start <= through);
        out.extend(register.ranges[begin..end].iter().map(|range| range.ordinal));
    }
}

impl Register {
    fn covering(&self, node: usize, start: usize, end: usize, prefix: usize, pc: usize, out: &mut Vec<usize>) {
        if start >= prefix || self.max_end[node] <= pc { return; }
        if end - start == 1 {
            out.push(self.ranges[start].ordinal);
        } else {
            let middle = start + (end - start) / 2;
            self.covering(node * 2, start, middle, prefix, pc, out);
            self.covering(node * 2 + 1, middle, end, prefix, pc, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_queries_match_linear_metadata_with_overlap_and_invalid_lifetimes() {
        let mut ranges = vec![(0, 0, 1000), (0, 3, 3), (0, 9, 2), (2, 1, 7)];
        ranges.extend((0..250).map(|i| ((i % 4) as u8, i * 2, i * 2 + 3)));
        ranges.reverse(); // Compiler order need not be sorted by start.
        let index = RegisterRanges::new(ranges.iter().copied());
        for register in 0..5 {
            for pc in 0..505 {
                let mut found = Vec::new();
                index.covering(register, pc, &mut found);
                found.sort_unstable();
                let expected: Vec<_> = ranges.iter().enumerate().filter_map(|(i, &(r, start, end))|
                    (r == register && start <= pc && pc < end).then_some(i)).collect();
                assert_eq!(found, expected);
                found.clear();
                index.starting_after_through(register, pc, pc + 5, &mut found);
                found.sort_unstable();
                let expected: Vec<_> = ranges.iter().enumerate().filter_map(|(i, &(r, start, _))|
                    (r == register && pc < start && start <= pc + 5).then_some(i)).collect();
                assert_eq!(found, expected);
            }
        }
    }
}
