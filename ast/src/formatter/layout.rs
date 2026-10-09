//! Line layout: which groups print on one line, how much room a call's
//! arguments get, and how a long `and`/`or` condition wraps. Every decision
//! is one flat preview bounded by a line's width and the preview's value
//! budget, so formatting stays linear in the output size.

use std::fmt::{self, Write};

use super::{FlatWidth, Formatter, IndentationMode, PREFERRED_LINE_WIDTH};
use crate::{BinaryOperation, Literal, RValue, Select, Table, UnaryOperation};

/// A table printed on one line by the short-table rule ends by this column.
const ONE_LINE_TABLE_COLUMN: usize = 100;
/// A call's arguments get at least this many columns wherever it starts, so
/// a short call deep in a UI tree stays on one line.
const MIN_GROUP_WIDTH: usize = 60;
/// A one-line table holds calls at most this long.
const SHORT_CALL_WIDTH: usize = 30;
/// Short-table rule: at most this many keyed entries...
const SHORT_KEYED_ENTRIES: usize = 4;
/// ...or this many array elements, every one atomic.
const SHORT_ARRAY_ENTRIES: usize = 8;

/// How a call lays out its arguments.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Arguments {
    /// On the call's line (a trailing function's body breaks on its own).
    Flat,
    /// On the call's line up to a trailing table constructor, which puts one
    /// entry per line: `f(a, {`.
    Hug,
    /// One argument per line.
    Lines,
}

/// How a flat preview ended.
#[derive(Clone, Copy)]
pub(super) struct Preview {
    /// Everything printed within the room, on one line.
    pub(super) fits: bool,
    /// A line break came before the room ran out.
    pub(super) multiline: bool,
}

/// The operands of a left-nested `and`/`or` chain, each with whether it
/// prints parenthesized.
struct Spine<'v> {
    operation: BinaryOperation,
    operands: Vec<(&'v RValue, bool)>,
}

impl<'v> Spine<'v> {
    /// `a or b or c` (left-nested, as conditions are normalized); a
    /// lower-precedence group stays one parenthesized operand.
    fn of(value: &'v RValue) -> Option<Self> {
        let RValue::Binary(top) = value else { return None };
        if !matches!(top.operation, BinaryOperation::And | BinaryOperation::Or) {
            return None;
        }
        let mut operands = Vec::new();
        let mut node = top;
        loop {
            operands.push((node.right.as_ref(), node.right_group()));
            match node.left.as_ref() {
                RValue::Binary(left) if left.operation == top.operation => node = left,
                left => {
                    operands.push((left, node.left_group()));
                    break;
                }
            }
        }
        operands.reverse();
        Some(Self { operation: top.operation, operands })
    }
}

impl<W: fmt::Write> Formatter<'_, W> {
    /// The display column the next character prints at: a tab of
    /// indentation counts four, as in an editor.
    fn column(&self) -> usize {
        let indentation_width = match self.indentation_mode {
            IndentationMode::Spaces(n) => usize::from(n),
            IndentationMode::Tab => 4,
        };
        self.position_query.map_or(self.indentation_level * indentation_width, |query| {
            let column = query(self.output).column_one_based.saturating_sub(1);
            // Source positions count a tab as one character.
            column + if matches!(self.indentation_mode, IndentationMode::Tab) { self.indentation_level * 3 } else { 0 }
        })
    }

    /// Renders `render` flat into `room` columns, stopping at the first line
    /// break or the edge. A preview never asks for another preview.
    pub(super) fn preview(
        &self,
        room: usize,
        render: impl FnOnce(&mut Formatter<'_, FlatWidth>) -> fmt::Result,
    ) -> Preview {
        let mut width = FlatWidth { remaining: room, already_multiline: false };
        let mut preview = Formatter {
            indentation_level: self.indentation_level,
            indentation_mode: match self.indentation_mode {
                IndentationMode::Spaces(n) => IndentationMode::Spaces(n),
                IndentationMode::Tab => IndentationMode::Tab,
            },
            output: &mut width,
            colon_method_calls: Default::default(),
            position_query: None,
            closure_observer: None,
            emission_map: None,
            layout_budget: Some(256),
            compact_annotations: self.compact_annotations,
            inferred_calls: Default::default(),
        };
        let fits = render(&mut preview).is_ok();
        Preview { fits, multiline: width.already_multiline }
    }

    /// The group fits the line, or an existing constructor or callback layout
    /// breaks it after an opening line that fits: nested groups get their own
    /// room during real emission, literal payload lines stay untouched.
    pub(super) fn fits_flat(&self, render: impl FnOnce(&mut Formatter<'_, FlatWidth>) -> fmt::Result) -> bool {
        let preview = self.preview(PREFERRED_LINE_WIDTH.saturating_sub(self.column()), render);
        preview.fits || preview.multiline
    }

    /// How a call of `arguments` lays them out, with `max(120 - column, 60)`
    /// columns for the call: on its line when they fit, or when the line up to
    /// a trailing function's first break does (`f(a, function(...)`); else
    /// hugging a trailing table when the line up to its `{` fits; else one
    /// argument per line. `render` prints the call laid out as asked.
    pub(super) fn argument_layout(
        &self,
        arguments: &[RValue],
        render: impl Fn(&mut Formatter<'_, FlatWidth>, Arguments) -> fmt::Result,
    ) -> Arguments {
        if self.layout_budget.is_some() || arguments.len() < 2 {
            return Arguments::Flat;
        }
        let room = PREFERRED_LINE_WIDTH.saturating_sub(self.column()).max(MIN_GROUP_WIDTH);
        let fits = |layout| {
            let preview = self.preview(room, |preview| render(preview, layout));
            preview.fits || preview.multiline
        };
        if fits(Arguments::Flat) {
            Arguments::Flat
        } else if matches!(arguments.last(), Some(RValue::Table(table)) if !table.0.is_empty()) && fits(Arguments::Hug) {
            Arguments::Hug
        } else {
            Arguments::Lines
        }
    }

    /// Whether a constructor prints one entry per line. A short table of
    /// atomic entries (at most 4 keyed or 8 array entries) stays on one line
    /// when it ends by column 100. Up to three array elements share a line
    /// when they fit it, one element always hugs (`{ X({`), and two or more
    /// go one per line once any of them spans lines.
    pub(super) fn table_spans_lines(&self, table: &Table, sequential_keys: bool) -> bool {
        let entries = table.0.len();
        if entries == 0 {
            return false;
        }
        let short = Self::short_table_shape(table, sequential_keys);
        let few = sequential_keys && entries <= 3 && !Self::contains_table(table);
        if self.layout_budget.is_some() {
            // A preview measures the shapes that may stay flat as flat.
            return !(short || few);
        }
        if short
            && self.preview(ONE_LINE_TABLE_COLUMN.saturating_sub(self.column()), |preview| preview.format_table(table)).fits
            && table.0.iter().all(|(_, value)| self.short_enough(value))
        {
            return false;
        }
        if few {
            return entries > 1
                && !self.preview(PREFERRED_LINE_WIDTH.saturating_sub(self.column()), |preview| preview.format_table(table)).fits;
        }
        true
    }

    /// All keyed entries (at most 4, not a renumbered array) or all array
    /// elements (at most 8), keys and values atomic.
    fn short_table_shape(table: &Table, sequential_keys: bool) -> bool {
        let limit = if sequential_keys { SHORT_ARRAY_ENTRIES } else { SHORT_KEYED_ENTRIES };
        table.0.len() <= limit
            && table.0.iter().all(|(key, value)| {
                Self::atomic(value)
                    && match key {
                        None => sequential_keys,
                        Some(key) => sequential_keys || Self::atomic(key),
                    }
            })
    }

    /// A literal, a name, a field chain, a negated number, an empty table, or
    /// a call of atomic arguments (its length is checked in emission).
    fn atomic(value: &RValue) -> bool {
        match value {
            RValue::Literal(_) | RValue::Local(_) | RValue::Global(_) => true,
            RValue::Index(index) => Self::atomic(&index.left) && Self::atomic(&index.right),
            RValue::Unary(unary) => {
                unary.operation == UnaryOperation::Negate
                    && matches!(unary.value.as_ref(), RValue::Literal(Literal::Number(_) | Literal::Integer(_)))
            }
            RValue::Table(table) => table.0.is_empty(),
            RValue::Call(call) | RValue::Select(Select::Call(call)) => {
                Self::atomic(&call.value) && call.arguments.iter().all(Self::atomic)
            }
            RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
                Self::atomic(&call.value) && call.arguments.iter().all(Self::atomic)
            }
            _ => false,
        }
    }

    /// A call in a one-line table is at most 30 characters long.
    fn short_enough(&self, value: &RValue) -> bool {
        !matches!(value, RValue::Call(_) | RValue::MethodCall(_) | RValue::Select(_))
            || self.preview(SHORT_CALL_WIDTH, |preview| preview.format_rvalue(value)).fits
    }

    /// `keyword condition closer` on one line, or, when that overflows the
    /// line and the condition is an `and`/`or` chain, StyLua's layout: the
    /// keyword alone, one operand per indented line led by its operator, the
    /// closer alone (`until` has none). The token sequence never changes.
    pub(super) fn format_condition_header(
        &mut self,
        keyword: &str,
        condition: &RValue,
        closer: Option<&str>,
    ) -> fmt::Result {
        let Some(spine) = self.overflowing_spine(condition, keyword, closer.unwrap_or("")) else {
            write!(self.output, "{keyword} ")?;
            self.format_rvalue(condition)?;
            if let Some(closer) = closer {
                write!(self.output, " {closer}")?;
            }
            return Ok(());
        };
        write!(self.output, "{keyword}")?;
        self.indentation_level += 1;
        for (index, &(operand, parenthesized)) in spine.operands.iter().enumerate() {
            writeln!(self.output)?;
            self.indent()?;
            if index != 0 {
                write!(self.output, "{} ", spine.operation)?;
            }
            self.format_chain_operand(operand, parenthesized)?;
        }
        self.indentation_level -= 1;
        if let Some(closer) = closer {
            writeln!(self.output)?;
            self.indent()?;
            write!(self.output, "{closer}")?;
        }
        Ok(())
    }

    /// An assigned or returned `and`/`or` chain that overflows the line
    /// hangs one operand per line, each led by its operator one level deeper.
    pub(super) fn format_hanging_value(&mut self, value: &RValue) -> fmt::Result {
        if self.overflowing_spine(value, "", "").is_none() {
            return self.format_rvalue(value);
        }
        self.indentation_level += 1;
        let result = self.format_chain_operand(value, false);
        self.indentation_level -= 1;
        result
    }

    /// An operand that starts the current line of a wrapped chain. When it is
    /// itself a chain (an `and` chain under an `or`, which needs no
    /// parentheses) and runs past the line, its operands continue on lines of
    /// their own at the same depth, each led by its operator.
    fn format_chain_operand(&mut self, operand: &RValue, parenthesized: bool) -> fmt::Result {
        let spine = (!parenthesized).then(|| self.overflowing_spine(operand, "", "")).flatten();
        let Some(spine) = spine else {
            return self.format_operand(operand, parenthesized);
        };
        for (index, &(operand, parenthesized)) in spine.operands.iter().enumerate() {
            if index != 0 {
                writeln!(self.output)?;
                self.indent()?;
                write!(self.output, "{} ", spine.operation)?;
            }
            self.format_chain_operand(operand, parenthesized)?;
        }
        Ok(())
    }

    /// The chain `value` is, when `keyword value closer` (either may be
    /// empty) runs past the line. One already broken by a function or table
    /// keeps its layout.
    fn overflowing_spine<'v>(&self, value: &'v RValue, keyword: &str, closer: &str) -> Option<Spine<'v>> {
        let RValue::Binary(binary) = value else { return None };
        if !matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) || self.layout_budget.is_some() {
            return None;
        }
        let preview = self.preview(PREFERRED_LINE_WIDTH.saturating_sub(self.column()), |preview| {
            if !keyword.is_empty() {
                write!(preview.output, "{keyword} ")?;
            }
            preview.format_rvalue(value)?;
            if !closer.is_empty() {
                write!(preview.output, " {closer}")?;
            }
            Ok(())
        });
        (!preview.fits && !preview.multiline).then(|| Spine::of(value)).flatten()
    }

    fn format_operand(&mut self, operand: &RValue, parenthesized: bool) -> fmt::Result {
        if parenthesized {
            self.output.write_char('(')?;
        }
        self.format_rvalue(operand)?;
        if parenthesized {
            self.output.write_char(')')?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    use crate::{
        Assign, Binary, BinaryOperation, Block, Call, Closure, Comment, Function, Global, If, Index, LValue,
        Literal, Local, MethodCall, RValue, RcLocal, Return, Select, Statement, Table, While,
    };

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn string(value: &str) -> RValue {
        Literal::String(value.as_bytes().to_vec()).into()
    }

    fn number(value: f64) -> RValue {
        Literal::Number(value).into()
    }

    fn field(base: RValue, name: &str) -> RValue {
        Index::new(base, string(name)).into()
    }

    /// `callee` may be a dotted path (`UDim2.fromScale`).
    fn call(callee: &str, arguments: Vec<RValue>) -> RValue {
        let mut names = callee.split('.');
        let root = global(names.next().unwrap());
        Call::new(names.fold(root, field), arguments).into()
    }

    fn binary(left: RValue, operation: BinaryOperation, right: RValue) -> RValue {
        Binary::new(left, right, operation).into()
    }

    fn keyed(entries: &[(&str, RValue)]) -> RValue {
        Table::new(entries.iter().map(|(key, value)| (Some(string(key)), value.clone())).collect()).into()
    }

    fn array(values: Vec<RValue>) -> RValue {
        Table::new(values.into_iter().map(|value| (None, value)).collect()).into()
    }

    fn declare(name: &str, value: RValue) -> Statement {
        Assign { prefix: true, ..Assign::new(vec![LValue::Local(local(name))], vec![value]) }.into()
    }

    fn closure(body: Vec<Statement>, native: bool) -> RValue {
        let function = Function { body: Block(body), native, ..Default::default() };
        RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(function))),
            upvalues: Vec::new(),
        })
    }

    /// `statement` inside `depth` loops, so it prints `depth` tabs deep.
    fn nested(statement: Statement, depth: usize) -> Block {
        let mut block = Block(vec![statement]);
        for _ in 0..depth {
            block = Block(vec![While::new(global("running"), block).into()]);
        }
        block
    }

    fn returned(value: RValue) -> String {
        Block(vec![Return::new(vec![value]).into()]).to_string()
    }

    /// Short tables of atomic entries print on one line, never past column 100.
    #[test]
    fn short_tables_stay_on_one_line() {
        let gem = keyed(&[("Asset", string("Gem")), ("Amount", number(500.0))]);
        assert_eq!(returned(gem), r#"return { Asset = "Gem", Amount = 500 }"#);
        let four = keyed(&[("a", number(1.0)), ("b", number(2.0)), ("c", number(3.0)), ("d", number(4.0))]);
        assert_eq!(returned(four), "return { a = 1, b = 2, c = 3, d = 4 }");
        let five = keyed(&[("a", number(1.0)), ("b", number(2.0)), ("c", number(3.0)), ("d", number(4.0)), ("e", number(5.0))]);
        assert!(returned(five).starts_with("return {\n\ta = 1,"));
        let eight = array((1..=8).map(|n| number(n as f64)).collect());
        assert_eq!(returned(eight), "return { 1, 2, 3, 4, 5, 6, 7, 8 }");
        let nine = array((1..=9).map(|n| number(n as f64)).collect());
        assert!(returned(nine).starts_with("return {\n\t1,"));
        let calls = keyed(&[("Size", call("UDim2.fromScale", vec![number(1.0), number(0.0)])), ("Empty", array(vec![]))]);
        assert_eq!(returned(calls), "return { Size = UDim2.fromScale(1, 0), Empty = {} }");
        // A nested table, a function or a mixed table keeps one entry per line.
        let nested_table = keyed(&[("inner", keyed(&[("x", number(1.0))]))]);
        assert!(returned(nested_table).starts_with("return {\n"));
        let callback = keyed(&[("run", closure(vec![], false))]);
        assert!(returned(callback).starts_with("return {\n"));
        let mixed = RValue::Table(Table::new(vec![(None, number(1.0)), (Some(string("x")), number(2.0))]));
        assert!(returned(mixed).starts_with("return {\n"));
        // A line that would end past column 100 does not stay one line.
        let long = keyed(&[("Message", string(&"m".repeat(70))), ("Code", number(404.0))]);
        assert!(returned(long).starts_with("return {\n"));
        // 48 columns of indentation: the 56-column table would end at 104.
        let message = || keyed(&[("Message", string(&"m".repeat(40)))]);
        let deep = nested(Return::new(vec![message()]).into(), 12).to_string();
        assert!(deep.contains("return {\n"), "{deep}");
        let shallow = nested(Return::new(vec![message()]).into(), 9).to_string();
        assert!(shallow.contains("return { Message = "), "{shallow}");
    }

    /// Two or more elements go one per line once any spans lines; a single
    /// element keeps hugging its braces.
    #[test]
    fn a_multi_line_element_puts_every_element_on_its_own_line() {
        let child = |name: &str| -> RValue {
            let mut entries: Vec<(&str, RValue)> = vec![("Name", string(name))];
            entries.extend([("A", number(1.0)), ("B", number(2.0)), ("C", number(3.0)), ("D", number(4.0))]);
            call("New", vec![keyed(&entries)])
        };
        let two = returned(array(vec![child("first"), child("second")]));
        assert!(two.starts_with("return {\n\tNew({\n"), "{two}");
        assert!(two.contains("\n\t}),\n\tNew({\n"), "{two}");
        assert!(two.ends_with("\n\t})\n}"), "{two}");
        let one = returned(array(vec![child("only")]));
        assert!(one.starts_with("return { New({\n"), "{one}");
    }

    /// A call short enough for 60 columns stays on one line however deep it
    /// starts; a trailing function hugs; an overlong call still breaks.
    #[test]
    fn call_arguments_get_sixty_columns_and_hug_a_trailing_function() {
        let weight = field(field(global("Enum"), "FontWeight"), "ExtraBold");
        let font = declare("font", call("Font.fromName", vec![string("Montserrat"), weight]));
        let deep = nested(font, 22).to_string();
        assert!(deep.contains(r#"local font = Font.fromName("Montserrat", Enum.FontWeight.ExtraBold)"#), "{deep}");
        let hug = Statement::from(Call::new(global("ForPairs"), vec![
            field(global("shopDataProcessor"), "ItemSet"),
            closure(vec![Call::new(global("print"), vec![number(1.0)]).into()], false),
        ]));
        let text = nested(hug, 10).to_string();
        assert_eq!(text.lines().nth(10).unwrap().trim(), "ForPairs(shopDataProcessor.ItemSet, function()", "{text}");
        let wide = Statement::from(Call::new(global("report"), vec![string(&"x".repeat(100)), string(&"y".repeat(100))]));
        assert!(Block(vec![wide]).to_string().starts_with("report(\n\t\""));
    }

    /// A long `and`/`or` condition wraps StyLua-style, one operand per line;
    /// a short one does not; a lower-precedence group stays parenthesized.
    #[test]
    fn long_conditions_wrap_one_operand_per_line() {
        let state = |name: &str| {
            binary(global("state"), BinaryOperation::Equal, field(field(global("Enum"), "HumanoidStateType"), name))
        };
        let any = binary(binary(state("FallingDown"), BinaryOperation::Or, state("Freefall")), BinaryOperation::Or, state("Jumping"));
        let branch = If::new(any.clone(), Block(vec![Call::new(global("jump"), vec![]).into()]), Block::default());
        assert_eq!(
            Block(vec![branch.into()]).to_string(),
            "if\n\tstate == Enum.HumanoidStateType.FallingDown\n\tor state == Enum.HumanoidStateType.Freefall\n\
             \tor state == Enum.HumanoidStateType.Jumping\nthen\n\tjump()\nend"
        );
        let short = If::new(binary(global("a"), BinaryOperation::Or, global("b")), Block::default(), Block::default());
        assert_eq!(Block(vec![short.into()]).to_string(), "if a or b then\nend");
        // `ready and (long or chain)`: the group is one operand, kept intact.
        let grouped = binary(global("ready"), BinaryOperation::And, any);
        let looped = While::new(grouped, Block(vec![Call::new(global("step"), vec![]).into()]));
        let text = Block(vec![looped.into()]).to_string();
        assert!(text.starts_with("while\n\tready\n\tand (state == Enum.HumanoidStateType.FallingDown or "), "{text}");
        assert!(text.ends_with(")\ndo\n\tstep()\nend"), "{text}");
    }

    /// An assigned or returned chain hangs; an `and` chain under an `or` that
    /// still overflows continues one operand per line at the same depth.
    #[test]
    fn assigned_chains_hang_their_operators() {
        let long = |name: &str| call(name, vec![string(&"v".repeat(40))]);
        let value = binary(
            binary(binary(global("enabled"), BinaryOperation::And, long("first")), BinaryOperation::And, long("second")),
            BinaryOperation::Or,
            string("0"),
        );
        let text = Block(vec![declare("label", value.clone())]).to_string();
        assert_eq!(text, format!(
            "local label = enabled\n\tand first(\"{0}\")\n\tand second(\"{0}\")\n\tor \"0\"",
            "v".repeat(40)
        ));
        let text = returned(value);
        assert!(text.starts_with("return enabled\n\tand first("), "{text}");
    }

    /// Printing fixes: no truncation parentheses around a backtick string, a
    /// comparison inside a comparison parenthesized, `@native` and
    /// `--!native` from the prototype flags.
    #[test]
    fn printing_fixes() {
        let interpolated = |format: &str| -> RValue {
            Select::MethodCall(MethodCall::new(string(format), "format".into(), vec![global("p")])).into()
        };
        assert_eq!(returned(interpolated("Secret %*")), "return `Secret {p}`");
        assert_eq!(returned(call("warn", vec![interpolated("Unable %*")])), "return warn(`Unable {p}`)");
        assert_eq!(returned(array(vec![interpolated("x%*")])), "return { `x{p}` }");
        assert_eq!(returned(interpolated("count: %d")), r#"return (("count: %d"):format(p))"#);
        let chained = binary(binary(global("a"), BinaryOperation::Equal, global("b")), BinaryOperation::Equal, Literal::Boolean(true).into());
        assert_eq!(returned(chained), "return (a == b) == true");
        let block = Block(vec![
            Comment::hot("native").into(),
            declare("f", closure(vec![], true)),
            declare("g", closure(vec![Return::new(vec![number(1.0)]).into()], true)),
        ]);
        assert_eq!(block.to_string(), "--!native\n@native local function f() end\n\n@native local function g()\n\treturn 1\nend");
        assert_eq!(returned(closure(vec![], true)), "return @native function() end");
    }
}
