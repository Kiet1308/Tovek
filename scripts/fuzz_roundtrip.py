#!/usr/bin/env python3
"""Typed random programs, decompiled and checked against their own bytecode.

Each seed generates a program from a typed grammar whose features come from
the bug classes reviews have found: captured cells written between operands,
metamethods and method calls that log their order, multiple results and
varargs, constructors with signed-zero and nil keys, NaN comparisons, loops
with `break`/`continue`, closures created in loops, helpers next to inline
copies of their bodies (de-inlining), shadowed libraries, `debug.info`,
register pressure. The reference is the compiled bytecode itself, run on the
benchmark VM; the decompiled source is compiled again (at a random level) and
run on the same VM with the same driver. `--mutate` also patches the bytecode
the way only a hand-made chunk can (a string no identifier spells, a NaN
payload): the decompiler must then refuse or keep the behavior.

Two capture families come from a random stream of their own, so a seed that
draws neither builds exactly the program it built before they existed:
`capture-factory` assigns a closure over a variable that shares a value with
what the closure captures (through a factory Luau -O2 inlines, or as a call
argument at every level), and `recursive-arm` defines a recursive local
function in one arm of a value branch.

Six de-inline families come from a third stream the same way. Each defines
a helper Luau -O2 inlines (a plain call below -O2) beside code shaped like
its copies: `written-param` writes its parameter, with copies that write a
caller's local read after an enclosing `if`, around a loop or in a `while`
condition (or dead); `returned-cell` changes outer state and returns an
outer local to a statement that reads other outer state first;
`error-level` calls `error(message, level)` under `pcall` and records
whether the message has a position; `closure-identity` compares closures
made in a loop with `==` (DUPCLOSURE against NEWCLOSURE), partly in the
main chunk; `service-handle` passes GetService/require handles (global
stubs) to the helper; `constant-args` passes constants that the inlined
copies fold (a pure helper's copy to one number, `1 - k` in a statement
helper's), beside the same numbers written by hand.

A failure keeps its directory and, with `--reduce`, a reduced program (whole
units deleted while the failure category stays). Passing seeds leave nothing.
"""
import argparse
import collections
import concurrent.futures
import json
import os
import pathlib
import random
import re
import shutil
import struct
import subprocess
import sys

VERSION = "typed-families-v3"

PRELUDE = r'''local ids, nextId = {}, 0
local function describe(value)
    local kind = type(value)
    if kind == "number" then
        if value ~= value then return "nan" end
        if value == 0 then return if 1 / value < 0 then "-0" else "0" end
        return tostring(value)
    elseif kind == "string" then
        return string.format("%q", (string.gsub(value, "^[%w_@./]-:%d+: ", "")))
    elseif kind == "table" then
        if not ids[value] then
            nextId += 1
            ids[value] = nextId
            local keys = {}
            for key, item in next, value do
                keys[#keys + 1] = describe(key) .. "=" .. (if type(item) == "table" then "t" else describe(item))
            end
            table.sort(keys)
            return "t" .. nextId .. "{" .. table.concat(keys, ",") .. "}"
        end
        return "t" .. ids[value]
    elseif kind == "function" then
        return "fn"
    end
    return tostring(value)
end
local log = {}
local function record(...)
    local parts = {}
    for index = 1, select("#", ...) do
        parts[index] = describe((select(index, ...)))
    end
    log[#log + 1] = select("#", ...) .. ":" .. table.concat(parts, ",")
    return ...
end
local function body(input, flip, ...)
    local a, b = input, if flip then 1 else -1
    local cell = input
    local list = {1, 2, 3}
    local rows = {4, 5, 6}
    local store = {x = 1, y = 2}
    local box = setmetatable({}, {
        __index = function(_, key) record("get", key) return store[key] end,
        __newindex = function(_, key, item) record("set", key, item) store[key] = item end,
    })
    local meta = setmetatable({}, {
        __add = function(left, right) record("add") return 5 end,
        __sub = function(left, right) cell = cell + 1 return 6 end,
        __eq = function() record("eq") return true end,
        __lt = function() record("lt") return false end,
        __len = function() record("len") return 7 end,
        __concat = function() record("concat") return "m" end,
        __call = function(_, item) record("call", item) return item end,
    })
    local function bump(amount)
        cell = cell + amount
        record("bump", cell)
        return amount
    end
    local function multi(count)
        record("multi", count)
        return table.unpack({cell, 2, 3}, 1, count)
    end
    local object = {k = 1}
    function object:get(amount) record("objget", self.k, amount) return self.k + amount end
    function object:set(amount) record("objset", amount) self.k = amount return self end
    local function iterate(limit)
        local index = 0
        return function()
            index += 1
            if index <= limit then return index end
            return nil
        end
    end
'''

ENDING = r'''    return a, b, cell, list[1], store.x
end
return function(input, flip)
    log, ids, nextId = {}, {}, 0
    local results = table.pack(pcall(body, input, flip, input, nil))
    local parts = {}
    for index = 1, results.n do parts[index] = describe(results[index]) end
    return table.concat(log, ";") .. " => " .. table.concat(parts, ",")
end
'''

DRIVER = r'''for _, input in {-3, -0.0, 0, 2.5, 1 / 0, 0 / 0} do
    for _, flip in {false, true} do
        print(f(input, flip))
    end
end
'''

FAMILIES = ("arithmetic", "capture", "metamethod", "method", "multret", "table", "control", "closure",
            "deinline", "shadow", "frames", "pressure")

# Drawn independently of FAMILIES, each with this chance, from the seed's
# own capture stream (see `Generator.capture_units`).
CAPTURE_FAMILIES = ("capture-factory", "recursive-arm")
CAPTURE_CHANCE = 0.2
CAPTURE_SHAPES = ("branch", "two-branches", "second-use", "loop", "parallel")
CLOSURE_BINDERS = ("factory", "keep", "record")
RECURSIVE_ARMS = ("then", "else", "diamond", "diamond-value")

# Helpers Luau -O2 really inlines, in the shapes the de-inliner rebuilds as
# calls. Drawn like the capture families, each with this chance, from a third
# stream of the seed (see `Generator.deinline_units`).
DEINLINE_FAMILIES = ("written-param", "returned-cell", "error-level", "closure-identity", "service-handle",
                     "constant-args")
DEINLINE_CHANCE = 0.15
# Where the caller's local stands that a copy of the helper body writes.
WRITTEN_PARAM_SITES = ("after-if", "loop", "while", "same-block", "dead")
# A conditional write (its test also guards the `while` site).
PARAM_WRITES = (("{v} > 100", "{v} = {v} - 100"), ("{v} < 0", "{v} = -{v}"), ("{v} ~= {v}", "{v} = 0"))
# The statement reading what the uniform-cell helper returns.
CELL_HOSTS = ("tag-first", "count-first", "cell-first", "concat", "constructor", "length", "direct")
ERROR_LEVELS = ("", ", 1", ", 2", ", 3", ", 0")
IDENTITY_SHAPES = ("chunk", "computed", "function")
HANDLE_SITES = ("declared", "statement-between", "declaration-between", "argument", "require", "twice")
# A pure helper's body (`n` its parameter): `^`, `//` and `%` fold too, but
# only some through rules a rebuilt call may rely on.
PURE_BODIES = ("n / 60", "n * 2.5", "-n * 3", "n - 0.25", "(n + 1) / 4", "n // 2", "n % 7", "n ^ 2")
# Its arguments: constants, signed zero, a huge value, and a variable.
CONSTANT_ARGUMENTS = ("13", "21", "6", "0.5", "-3", "0", "-0", "143", "1e300", "input")
CONSTANT_SITES = ("call", "by-hand", "statement", "statement-by-hand")

REFUSALS = ("headroom for the vector constructor", "a method name no identifier spells",
            "a NaN constant whose payload", "a global name no identifier spells",
            "more locals at once than Luau allows", "more registers than Luau allows",
            "no faithful source", "a builtin call names another function", "a loop prepared for")

# `describe` prints every function as `fn`: only `closure-identity` observes
# closure identity (with `==`), which DUPCLOSURE shares only from -O1, and its
# seeds are recompiled within one regime (`run_seed`). A mutation renaming the
# string "function" sends functions to `tostring`, a heap address that differs
# between two runs of one chunk, so outputs compare with addresses as one token.
HEAP_ADDRESS = re.compile(r"\b(function|table|thread|userdata|buffer): 0x[0-9a-fA-F]+")


def comparable(output):
    return HEAP_ADDRESS.sub(r"\1: 0x", output)


class ChunkUnit(list):
    """The lines of a unit that runs in the main chunk, before `body`."""


class Generator:
    def __init__(self, seed):
        self.rng = random.Random(seed)
        # A second stream: drawing the capture families never shifts `rng`.
        self.capture_rng = random.Random(f"capture-families:{seed}")
        # A third, for the de-inline families: it shifts neither.
        self.deinline_rng = random.Random(f"deinline-families:{seed}")
        count = self.rng.randint(1, 4)
        self.families = set(self.rng.sample(FAMILIES, count))
        self.locals = []  # numeric locals in scope: list of scopes
        self.loop_depth = 0
        self.counter = 0
        self.helpers = []

    def has(self, family):
        return family in self.families

    def fresh(self, prefix="v"):
        self.counter += 1
        return f"{prefix}{self.counter}"

    def number_leaf(self):
        rng = self.rng
        pool = ["a", "b", "cell", "input"] + [name for scope in self.locals for name in scope]
        if rng.random() < 0.35:
            return rng.choice(["0", "1", "2", "-1", "0.5", "-0", "3", "10", "1e300", "255", "256", "-2.5"])
        return rng.choice(pool)

    def number(self, depth):
        rng = self.rng
        if depth <= 0 or rng.random() < 0.3:
            return self.number_leaf()
        sub = lambda: self.number(depth - 1)
        options = [
            lambda: self.arithmetic(sub),
            lambda: f"(- {sub()})",
            lambda: f"record({sub()})",
            lambda: f"(if {self.boolean(depth - 1)} then {sub()} else {sub()})",
            lambda: f"({self.boolean(depth - 1)} and {sub()} or {sub()})",
            lambda: f"#list",
            lambda: f"math.max({sub()}, {sub()})",
            lambda: f"math.floor({sub()})",
        ]
        if self.has("capture"):
            options += [lambda: f"bump({sub()})", lambda: f"({sub()} + bump({sub()}) + cell)",
                        lambda: f"(cell * bump(1))"]
        if self.has("metamethod"):
            options += [lambda: "(meta + 1)", lambda: "(meta - 1)", lambda: "#meta", lambda: f"meta({sub()})",
                        lambda: f"box.{rng.choice('xy')}"]
        if self.has("method"):
            options += [lambda: f"object:get({sub()})", lambda: f"object:set({sub()}):get({sub()})"]
        if self.has("multret"):
            options += [lambda: f'select("#", multi({rng.randint(0, 3)}))', lambda: f"(multi({rng.randint(1, 3)}))",
                        lambda: f"(multi({rng.randint(0, 3)}) or {sub()})",
                        lambda: f"select({rng.randint(1, 3)}, {sub()}, {sub()}, {sub()})"]
        if self.has("closure"):
            options += [lambda: f"(function(p) return p + {sub()} end)({sub()})"]
        if self.helpers:
            options += [lambda: f"{rng.choice(self.helpers)[0]}({sub()})"]
        return rng.choice(options)()

    def arithmetic(self, sub):
        rng = self.rng
        left = sub()
        operator = rng.choice(['+', '-', '*', '/', '//', '%', '^'])
        # Luau compiles `x ^ k` with a constant `k` to POWK from -O1 only, and
        # POWK computes `^ 2`, `^ 0.5` and `^ 3` without `pow` (`sqrt(-0)` is
        # -0, `pow(-0, 0.5)` is 0): the same source differs between levels.
        # An exponent is never one of those constants.
        right = rng.choice(["1.5", "-1", "0", "4", "-2.5", "a", "b", "input"]) if operator == "^" else sub()
        return f"({left} {operator} {right})"

    def boolean(self, depth):
        rng = self.rng
        if depth <= 0 or rng.random() < 0.25:
            return rng.choice(["flip", "(not flip)", f"({self.number_leaf()} < {self.number_leaf()})"])
        sub = lambda: self.boolean(depth - 1)
        num = lambda: self.number(depth - 1)
        options = [
            lambda: f"({num()} {rng.choice(['<', '<=', '>', '>=', '==', '~='])} {num()})",
            lambda: f"(not {sub()})",
            lambda: f"({sub()} {rng.choice(['and', 'or'])} {sub()})",
            lambda: f"record({sub()})",
        ]
        if self.has("metamethod"):
            options += [lambda: "(meta == setmetatable({}, getmetatable(meta)))", lambda: "(meta < meta)"]
        return rng.choice(options)()

    def string(self, depth):
        rng = self.rng
        num = lambda: self.number(depth - 1)
        return rng.choice([
            lambda: f'("s" .. {num()})',
            lambda: f"`<{{{num()}}}|{{{num()}}}>`",
            lambda: f'("%*-%*"):format({num()}, {num()})',
            lambda: f"tostring({num()})",
        ])()

    def value(self, depth):
        rng = self.rng
        choice = rng.random()
        if choice < 0.6:
            return self.number(depth)
        if choice < 0.75:
            return self.boolean(depth)
        if choice < 0.85:
            return self.string(depth)
        return rng.choice(["nil", "false", "list", "store"])

    def zero_keys(self):
        """How one table spells its zero key. Which sign the key keeps when a
        store spelled with the other sign lands on its slot holding `nil`
        depends on the hash layout, which the decompiler does not keep: a
        table spells `0` one way, or both ways with values that are never
        `nil`."""
        rng = self.rng
        spelling = rng.choice(["[0]", "[-0]", "both"])
        if spelling == "both":
            return ["[0]", "[-0]"], lambda: rng.choice(["1", "2.5", "-3", "true", '"z"'])
        return [spelling], None

    def keyed_value(self, key, zero, depth):
        if key in zero[0] and zero[1]:
            return zero[1]()
        return self.rng.choice([self.value(depth), "nil"])

    def table(self, depth, zero):
        rng = self.rng
        items = [self.value(depth - 1) for _ in range(rng.randint(0, 4))]
        keyed = []
        for _ in range(rng.randint(0, 4)):
            key = rng.choice(["k", "x", *zero[0], "[1]", "[2]", '["two words"]', "[true]"])
            keyed.append(f"{key} = {self.keyed_value(key, zero, depth - 1)}")
        if self.has("multret") and rng.random() < 0.4:
            items.append(f"multi({rng.randint(0, 3)})")
        elif rng.random() < 0.2:
            items.append("...")
        return "{" + ", ".join(keyed + items) + "}"

    def block(self, depth, count):
        self.locals.append([])
        lines = []
        for _ in range(count):
            lines += self.statement(depth)
        self.locals.pop()
        return lines

    def statement(self, depth):
        rng = self.rng
        num = lambda: self.number(2 if depth > 0 else 1)
        options = [
            lambda: self.declare(),
            lambda: [f"record({', '.join(self.value(2) for _ in range(rng.randint(1, 3)))})"],
            lambda: [f"cell = {num()}"],
            lambda: [f"{rng.choice(['a', 'b', 'cell'])} {rng.choice(['+=', '-=', '*='])} {num()}"],
            lambda: ["a, b = b, a"],
            lambda: [f"list[#list + 1] = {self.value(1)}"],
        ]
        if self.locals and any(self.locals):
            options.append(lambda: [f"{rng.choice([n for s in self.locals for n in s])} = {num()}"])
        if self.has("capture"):
            options += [lambda: [f"a, cell = cell, {num()}"], lambda: [f"record(cell, bump({num()}), cell)"],
                        lambda: [f"list[bump(1) % 3 + 1] = bump(2)"]]
        if self.has("metamethod"):
            options += [lambda: [f"box.{rng.choice('xyz')} = {num()}"], lambda: [f"box[bump(1)] = bump(2)"]
                        if self.has("capture") else [f"box.x = box.y"]]
        if self.has("method"):
            options += [lambda: [f"object:set({num()})"], lambda: [f"record(object:get({num()}))"]]
        if self.has("multret"):
            options += [lambda: [f"record(multi({rng.randint(0, 3)}))"],
                        lambda: [f"record(table.pack(multi({rng.randint(0, 3)})).n)"],
                        lambda: self.multi_declare()]
        if self.has("table"):
            options += [lambda: self.table_statements()]
        if self.has("shadow"):
            options += [lambda: self.shadow_statements()]
        if self.has("frames"):
            options += [lambda: self.frame_statements()]
        if depth > 0 and (self.has("control") or rng.random() < 0.3):
            options += [lambda: self.if_statement(depth), lambda: self.numeric_loop(depth),
                        lambda: self.generic_loop(depth), lambda: self.while_loop(depth)]
        if self.has("closure") and depth > 0:
            options += [lambda: self.closure_statements(depth)]
        if self.loop_depth > 0:
            options += [lambda: [f"if {self.boolean(1)} then {rng.choice(['break', 'continue'])} end"]]
        return rng.choice(options)()

    def declare(self):
        name = self.fresh()
        line = f"local {name} = {self.number(2)}"
        self.locals[-1].append(name)
        return [line]

    def multi_declare(self):
        names = [self.fresh() for _ in range(self.rng.randint(1, 3))]
        return [f"local {', '.join(names)} = multi({self.rng.randint(0, 3)})", f"record({', '.join(names)})"]

    def table_statements(self):
        rng = self.rng
        name = self.fresh("t")
        zero = self.zero_keys()
        lines = [f"local {name} = {self.table(2, zero)}"]
        for _ in range(rng.randint(0, 3)):
            key = rng.choice([".k", ".x", *zero[0], "[1]", "[3]", '["two words"]'])
            lines.append(f"{name}{key} = {self.keyed_value(key, zero, 1)}")
        # `#` of a table with `nil` holes is any border, and which one the
        # layout decides: the tables built here are recorded by contents.
        lines.append(f"record({name})")
        return lines

    def shadow_statements(self):
        rng = self.rng
        return rng.choice([
            ["local math = {max = function() return \"shadow\" end, floor = math.floor, huge = 7, pi = 3}",
             "record(math.max(1, 2), math.huge, math.pi, 1 / 0, 3.141592653589793)"],
            ["local vector = {create = function() return \"fake\" end}", "record(vector.create(1, 2, 3))"],
            ["local type = function() return \"shadow\" end", "record(type(1), typeof(1))"],
            [f"record(vector.create(1, 2, {self.number(1)}), vector.create(0, -0, 0.5))"],
        ])

    def frame_statements(self):
        name = self.fresh("probe")
        caller = self.fresh("caller")
        return [f"local function {name}() return debug.info(2, \"f\") end",
                f"local function {caller}(x) local seen = {name}() return seen == {caller}, x end",
                f"record({caller}({self.number(1)}), debug.info(1, \"s\"))"]

    def closure_statements(self, depth):
        rng = self.rng
        name = self.fresh("fn")
        param = self.fresh("p")
        self.locals.append([param])
        body = self.number(2)
        self.locals.pop()
        lines = [f"local function {name}({param})", f"    cell = cell + 1", f"    return {body}", "end",
                 f"record({name}({self.number(1)}), cell)"]
        if rng.random() < 0.5:
            fns = self.fresh("fns")
            lines += [f"local {fns} = {{}}", f"for i = 1, {rng.randint(1, 3)} do",
                      f"    {fns}[i] = function() return i + cell end", "end",
                      f"for _, g in ipairs({fns}) do record(g()) end"]
        return lines

    def if_statement(self, depth):
        lines = [f"if {self.boolean(2)} then"] + ["    " + l for l in self.block(depth - 1, self.rng.randint(1, 3))]
        if self.rng.random() < 0.4:
            lines += [f"elseif {self.boolean(2)} then"] + ["    " + l for l in self.block(depth - 1, 1)]
        if self.rng.random() < 0.5:
            lines += ["else"] + ["    " + l for l in self.block(depth - 1, self.rng.randint(1, 2))]
        return lines + ["end"]

    def loop_body(self, depth, variables):
        self.loop_depth += 1
        self.locals.append(variables)
        lines = self.block(depth - 1, self.rng.randint(1, 3))
        self.locals.pop()
        self.loop_depth -= 1
        return ["    " + l for l in lines]

    def numeric_loop(self, depth):
        rng = self.rng
        variable = self.fresh("i")
        # Luau -O2 unrolls a loop with constant bounds and computes its first
        # index as `start + 0 * step`, which turns `-0` into 0: a loop from
        # `-0` runs to a bound no compiler knows (`rows` never grows).
        start, stop, step = rng.choice([("1", "3", None), ("3", "1", "-1"), ("1", "2", "0.5"), ("0", "-2", "-1"),
                                        ("1", "#list", None), ("-0", "#rows", None)])
        header = f"for {variable} = {start}, {stop}" + (f", {step}" if step else "") + " do"
        return [header] + self.loop_body(depth, [variable]) + ["end"]

    def generic_loop(self, depth):
        rng = self.rng
        if rng.random() < 0.5:
            index, item = self.fresh("k"), self.fresh("w")
            # `list` grows in loop bodies; iterating it would never end.
            return [f"for {index}, {item} in ipairs(rows) do"] + self.loop_body(depth, [index]) + ["end"]
        item = self.fresh("w")
        return [f"for {item} in iterate({rng.randint(0, 3)}) do"] + self.loop_body(depth, [item]) + ["end"]

    def while_loop(self, depth):
        guard = self.fresh("guard")
        return [f"local {guard} = 0", f"while {guard} < {self.rng.randint(1, 4)} do", f"    {guard} += 1"] + \
            self.loop_body(depth, []) + ["end"]

    def helper_definitions(self):
        # A helper and, elsewhere, a copy of its body: Luau -O2 inlines the
        # calls, and the de-inliner looks for both shapes.
        lines = []
        for _ in range(self.rng.randint(1, 2)):
            name, param = self.fresh("helper"), self.fresh("x")
            self.locals.append([param])
            body = self.number(2)
            self.locals.pop()
            self.helpers.append((name, param, body))
            lines += [f"local function {name}({param})", f"    return {body}", "end"]
        return lines

    def inline_copies(self):
        lines = []
        for name, param, body in self.helpers:
            argument = self.rng.choice(["a", "b", "cell"])
            copy = re.sub(rf"\b{param}\b", argument, body)
            lines.append(f"record({name}({argument}), {copy})")
        return lines

    def pressure(self):
        # Many locals held at once and calls with long argument lists, close to
        # Luau's 200 locals and 255 registers.
        rng = self.rng
        count = rng.randint(120, 185)
        names = [self.fresh("r") for _ in range(count)]
        lines = [f"local {', '.join(names[i:i + 10])} = {', '.join('a' for _ in names[i:i + 10])}"
                 for i in range(0, count, 10)]
        for _ in range(rng.randint(1, 3)):
            arguments = ", ".join(rng.choice(names + [self.number(1) for _ in range(5)]) for _ in range(rng.randint(20, 60)))
            lines.append(f"record({arguments})")
        lines.append(f"record({' + '.join(names[:40])})")
        return lines

    def capture_units(self):
        """The capture families' units, drawn from `capture_rng` once every
        other unit exists, so the units drawn from `rng` stay what they were.
        `program` puts them before the inline copies and the pressure unit."""
        rng = self.capture_rng
        units = []
        if rng.random() < CAPTURE_CHANCE:
            self.families.add("capture-factory")
            units.append(self.capture_factory(rng.choice(CAPTURE_SHAPES), rng.choice(CLOSURE_BINDERS)))
        if rng.random() < CAPTURE_CHANCE:
            self.families.add("recursive-arm")
            units.append(self.recursive_arm(rng.choice(RECURSIVE_ARMS), rng.random() < 0.5))
        return units

    def bind_closure(self, binder, captured, arguments, result, call=None):
        """`function(arguments) return result end` over the locals named in
        `captured`, made the way `binder` says:

        * `factory`: a local factory taking `captured` (called with `call`,
          by default the same names). Luau -O2 inlines it, so the closure is
          made right in the assignment; below -O2 it is a plain call.
        * `keep`: a small local function given the literal. A call argument
          below -O2; -O2 inlines `keep` around it.
        * `record`: the variadic `record` given the literal. Never inlined,
          so the closure is a call argument at every level.

        Returns the definitions the binder needs and the expression."""
        if binder == "factory":
            make = self.fresh("make")
            definition = [f"local function {make}({', '.join(captured)})",
                          f"    return function({arguments})", f"        return {result}", "    end", "end"]
            return definition, f"{make}({', '.join(call or captured)})"
        literal = f"function({arguments}) return {result} end"
        if binder == "keep":
            keep, kept = self.fresh("keep"), self.fresh("kept")
            definition = [f"local {kept} = {{}}", f"local function {keep}(f)", f"    {kept}[#{kept} + 1] = f",
                          "    return f", "end"]
            return definition, f"{keep}({literal})"
        return [], f"record({literal})"

    def capture_factory(self, shape, binder):
        """A closure assigned over a variable that holds, on another path or
        in the same parallel copy, the value the closure captures. The
        decompiler must keep them apart: merged into one variable, the
        closure captures itself and recurses until the stack overflows."""
        if shape == "loop":
            return self.capture_loop(binder)
        if shape == "parallel":
            return self.capture_parallel(binder)
        # `local failure = reject; if tag then failure = wrap(tag, reject) end`
        # in a function the caller gets back, so no inlining reshapes it.
        definition, failure = self.bind_closure(binder, ["reject", "tag"], "x", "reject(x) * 100 + tag")
        pick = self.fresh("pick")
        arm = [f"failure = {failure}"]
        after = []
        if shape == "second-use":
            # The closure is also stored, so it stays a statement of its own
            # and a separate copy carries it to the join.
            last = self.fresh("last")
            definition.append(f"local {last} = {{}}")
            arm.append(f"{last}.last = failure")
            after.append(f"record({last}.last and {last}.last(3))")
        lines = ["local failure = reject", "if tag then", *("    " + line for line in arm), "end"]
        if shape == "two-branches":
            # Promise `_andThen`: an earlier branch of the same shape must not
            # treat `failure` as written (and snapshot what it captures).
            more, success = self.bind_closure(binder, ["resolve", "tag"], "x", "resolve(x) + tag")
            definition += more
            lines = ["local success = resolve", "if tag then", f"    success = {success}", "end", *lines,
                     "return sink(success, failure)"]
            parameters = "resolve, reject, sink"
            arguments = "function(v) return v + 1 end, function(v) return v * 10 end, " \
                        "function(s, f) return s(2), f(2) end"
        else:
            lines.append("return sink(failure)")
            parameters = "reject, sink"
            arguments = "function(v) return v * 10 end, function(f) return f(2) end"
        tag = self.capture_rng.choice(["3", "0.5", "-2"])
        return self.unit(definition + [
            f"local function {pick}(tag)", f"    return function({parameters})",
            *("        " + line for line in lines), "    end", "end",
            f"record({pick}(if flip then {tag} else nil)({arguments}))", *after])

    def capture_loop(self, binder):
        """`acc = wrap(i, acc)` in a loop: the closure captures the value the
        assignment overwrites. A literal needs that value in a local of its
        own (`previous`), or it would read `acc` itself; a factory's
        argument is already the copy. The loop is in a function reached
        through `record`, so its bound stays a parameter: Luau -O2 unrolls a
        loop with constant bounds (or a copy inlined with a constant count)
        into straight code without the shared variable."""
        chain = self.fresh("chain")
        definition, wrapped = self.bind_closure(binder, ["previous", "i"], "x", "previous(x) + i",
                                                call=["acc", "i"])
        rebind = [f"acc = {wrapped}"] if binder == "factory" else ["local previous = acc", f"acc = {wrapped}"]
        if self.capture_rng.random() < 0.5:
            loop = ["for i = 1, count do", *("    " + line for line in rebind), "end"]
        else:
            loop = ["local i = 0", "while i < count do", "    i += 1", *("    " + line for line in rebind), "end"]
        handler = self.fresh("handler")
        return self.unit(definition + [
            f"local function {chain}(count)", "    local acc = function(x) return x end",
            *("    " + line for line in loop), "    return acc", "end",
            f"local {handler} = record({chain})({self.capture_rng.randint(1, 3)})",
            f"record({handler}(100), {handler}(2.5))"])

    def capture_parallel(self, binder):
        """Two variables meet on one edge: `first` becomes a closure over
        `right` while `second` takes `left`, in one parallel copy. Reached
        through `record`, so the arguments stay parameters (an inlined copy
        folds the constants and the shared variables disappear)."""
        definition, made = self.bind_closure(binder, ["right"], "", "right")
        pair, first, second = self.fresh("pair"), self.fresh("first"), self.fresh("second")
        copy = [f"first, second = {made}, left"] if self.capture_rng.random() < 0.5 else \
            [f"first = {made}", "second = left"]
        return self.unit(definition + [
            f"local function {pair}(left, right, flag)", "    local first, second = left, right",
            "    if flag then", *("        " + line for line in copy), "    end",
            "    return first, second", "end",
            f"local {first}, {second} = record({pair})(\"a\", \"b\", flip)",
            f"record(if flip then {first}() else {first}, {second})"])

    def recursive_arm(self, arm, indirect):
        """A recursive local function defined in one arm of a value branch.
        Its closure captures its own register, so moving the closure out of
        the arm (into `flag and function ... end or other`) leaves the
        recursion calling a variable nothing set. Called directly, Luau -O2
        inlines the chooser into `body`; called `indirect`ly (through
        `record`), never."""
        choose, rec, chosen = self.fresh("choose"), self.fresh("rec"), self.fresh("chosen")
        recursive = [f"local function {rec}(n)", f"    if n <= 0 then return \"{rec}\" end",
                     f"    return {rec}(n - 1)", "end", f"chosen = {rec}"]
        recursive = ["    " + line for line in recursive]
        if arm in ("then", "else"):
            condition = "flag" if arm == "then" else "not flag"
            body = ["local chosen = fallback", f"if {condition} then", *recursive, "end"]
        else:
            other = "function() return \"plain\" end" if arm == "diamond" else "1"
            body = ["local chosen", "if flag then", *recursive, "else", f"    chosen = {other}", "end"]
        callee = f"record({choose})" if indirect else choose
        # `chosen` is the recursive function exactly when `flip` picks its
        # arm; the `diamond-value` arm's other value is not callable.
        use = f"if flip then {chosen}(3) else {chosen}" if arm == "diamond-value" else f"{chosen}(3)"
        return self.unit([f"local function {choose}(flag, fallback)", *("    " + line for line in body),
                          "    return chosen", "end",
                          f"local {chosen} = {callee}(flip, function() return \"fallback\" end)",
                          f"record({use})"])

    def deinline_units(self):
        """The de-inline families' units, drawn from `deinline_rng` after the
        capture families, so neither earlier stream shifts. Each family
        defines a helper that Luau -O2 inlines (a real call below -O2) and
        code shaped like its inlined copies; `program` puts the units with
        the capture families' ones."""
        rng = self.deinline_rng
        builders = (self.written_param, self.returned_cell, self.error_level, self.closure_identity,
                    self.service_handle, self.constant_args)
        units = []
        for family, build in zip(DEINLINE_FAMILIES, builders):
            if rng.random() < DEINLINE_CHANCE:
                self.families.add(family)
                units += build()
        return units

    def written_param(self):
        """A helper writing its parameter, beside copies of its body that
        write a caller's local itself. Such a copy stands for a call only
        where that local is dead afterwards (out-of-SSA coalesces an inlined
        parameter into it only then). Here the local is read after an
        enclosing `if`, around a loop or in a `while` condition (M2 review
        finding 1), later in its own block, or never again. Calls of the
        helper sit beside the copies: -O2 inlines them with the argument in
        a register of its own. A function holding a site is reached through
        `record`, so -O2 does not fold a constant argument into it. The
        write is conditional: an unconditional one folds into the use."""
        rng = self.deinline_rng
        out, helper = self.fresh("out"), self.fresh("put")
        value = rng.random() < 0.5
        test, change = rng.choice(PARAM_WRITES)
        write = lambda local: f"if {test.format(v=local)} then {change.format(v=local)} end"

        def site(local, at, copy):
            """The helper's body on `local` (a copy), or a call of it. A
            value helper's result goes to `s`: declared at the site, or the
            host's own `s`, which the host returns."""
            if not value:
                return [write(local), f"{out}[{at}] = tostring({local})"] if copy else [f"{helper}({at}, {local})"]
            result = f"\"<\" .. tostring({local}) .. \">\"" if copy else f"{helper}({local})"
            target = rng.choice(["local s", "s"])
            return [*([write(local)] if copy else []), f"{target} = {result}", f"{out}[{at}] = s"]

        lines = [f"local {out} = {{}}", f"local function {helper}({'v' if value else 'at, v'})", "    " + write("v"),
                 "    return \"<\" .. tostring(v) .. \">\"" if value else f"    {out}[at] = tostring(v)", "end",
                 *(["local s = \"-\""] if value else []), *site("input", 8, False)]
        indent = lambda block, depth=1: ["    " * depth + line for line in block]
        for _ in range(rng.randint(1, 3)):
            where, copy = rng.choice(WRITTEN_PARAM_SITES), rng.random() < 0.7
            argument = rng.choice(["input + 250", "input - 250", "input * 100", "250"])
            name = self.fresh("host")
            start = ["    local s = \"-\""] if value else []
            returned = ", s" if value else ""
            if where == "after-if":
                lines += [f"local function {name}(v, go)", *start, "    if go then", *indent(site("v", 9, copy), 2),
                          "    end", f"    return v{returned}", "end",
                          f"record(record({name})({argument}, flip))"]
            elif where == "loop":
                header = rng.choice(["for i = 1, count do", "for i in iterate(count) do"])
                lines += [f"local function {name}(v, count)", *start, f"    {header}",
                          *indent(site("v", "i", copy), 2), "    end", f"    return v{returned}", "end",
                          f"record(record({name})({argument}, {rng.randint(2, 3)}))"]
            elif where == "while":
                lines += [f"local function {name}(v)", *start, "    local n = 0",
                          f"    while {test.format(v='v')} and n < 3 do", "        n += 1",
                          *indent(site("v", "n", copy), 2), "    end", f"    return v, n{returned}", "end",
                          f"record(record({name})({argument}))"]
            elif where == "same-block":
                lines += [f"local function {name}(v)", *start, *indent(site("v", 6, copy)),
                          f"    return v{returned}", "end", f"record(record({name})({argument}))"]
            else:
                # Dead afterwards: a parameter, or a local of the same block.
                local = rng.choice(["v", "w"])
                lines += [f"local function {name}(v, k)", *start, "    local w = v + k",
                          *indent(site(local, 7, copy)), f"    return {out}[7]", "end",
                          f"record(record({name})({argument}, 1))"]
        # Every store the copies and calls made, read once they all ran.
        return [self.unit([*lines, f"record({out})"])]

    def returned_cell(self):
        """A helper changing outer state and returning an outer local (the
        same cell on every path), whose result the next statement reads
        after other outer state. Rebuilt inside that statement, the call
        runs after the values read before the cell, so it may only go there
        when none of them can see its changes (M2 review finding 2). The
        site is in the unit, or in a function -O2 inlines into it (or
        reaches through `record`, where -O2 inlines only the helper)."""
        rng = self.deinline_rng
        cell, tag, count, helper = self.fresh("list"), self.fresh("tag"), self.fresh("count"), self.fresh("refill")
        lines = [f"local {cell}, {tag}, {count} = {{}}, \"old\", 0", f"local function {helper}(x, y)"]
        lines += ["    " + line for line in rng.choice([
            [f"table.clear({cell})", f"{cell}[1] = x", f"{cell}[2] = y", f"{tag} = \"new:\" .. tostring(x)",
             f"return {cell}"],
            ["if x ~= x then", f"    {tag} = \"nan\"", f"    return {cell}", "end", f"{cell}[1] = x",
             f"{tag} = {tag} .. \"+\"", f"return {cell}"],
            [f"{count} += 1", f"{cell}[{count}] = x", f"{tag} = tostring(y)", f"return {cell}"],
        ])] + ["end"]
        for _ in range(rng.randint(1, 3)):
            host = rng.choice(CELL_HOSTS)
            result = self.fresh("l")
            call = f"{helper}(x, {rng.choice(['3', 'y'])})"
            read = {
                "tag-first": [f"record({tag}, {result}[1], {result}[2])"],
                "count-first": [f"record({count}, {result}[1])"],
                "cell-first": [f"record({result}[1], {tag}, {count})"],
                "concat": [f"local joined = {tag} .. \"|\" .. tostring({result}[1])", "record(joined)"],
                "constructor": [f"record({{{tag}, {result}[2], {count}}})"],
                "length": [f"record(#{result}, {tag})"],
                "direct": [],
            }[host]
            body = [f"record({tag}, {call}[1])"] if host == "direct" else [f"local {result} = {call}", *read]
            arguments = rng.choice(["input, flip", "5, 3", "flip and 6 or 7, input"])
            place = rng.choice(["block", "inlined", "reached"])
            if place == "block":
                lines += ["do", f"    local x, y = {arguments}", *("    " + line for line in body), "end"]
            else:
                show = self.fresh("show")
                callee = show if place == "inlined" else f"record({show})"
                lines += [f"local function {show}(x, y)", *("    " + line for line in body), "end",
                          f"{callee}({arguments})"]
        return [self.unit(lines)]

    def error_level(self):
        """A helper raising `error(message, level)`, called under `pcall`.
        In an -O2 copy, level 2 names the caller's caller; rebuilt as a
        call, the caller (M2 review finding 5). The message keeps whether it
        has a position (`@:`), not the position itself."""
        rng = self.deinline_rng
        check, setter = self.fresh("check"), self.fresh("set")
        message = rng.choice(["\"bad \" .. name", "{reason = name}"])
        lines = [f"local function {check}(v, name)",
                 f"    if typeof(v) ~= \"number\" then error({message}{rng.choice(ERROR_LEVELS)}) end",
                 "    record(\"ok\", name)", "end",
                 f"local function {setter}(v)", f"    {check}(v, \"speed\")", "    return v * 2", "end"]
        for _ in range(rng.randint(1, 2)):
            argument = rng.choice(["if flip then \"fast\" else input", "\"fast\"", "input"])
            protected = rng.choice([f"{setter}, {argument}", f"function() {check}({argument}, \"size\") end"])
            ok, problem = self.fresh("ok"), self.fresh("problem")
            lines += [f"local {ok}, {problem} = pcall({protected})",
                      f"record({ok}, if typeof({problem}) == \"string\" then "
                      f"(string.gsub({problem}, \"^[^:]*:%d+: \", \"@:\")) else {problem})"]
        return [self.unit(lines)]

    def closure_identity(self):
        """Closures made in a loop, compared with `==`. From -O1, DUPCLOSURE
        shares a literal whose captures are all unwritten main-chunk locals
        (or locals bound to such a literal); NEWCLOSURE makes one per trip.
        A local initialized once (a computed constant, or a local function)
        is captured beside a capture-free literal, and the -O2 copy of a
        closure-making helper (always NEWCLOSURE: it captures the helper's
        parameter) beside a literal over the same local. M2 review finding 3
        needs that local to be a register of the main chunk, so the `chunk`
        shape runs there (a `ChunkUnit`) and the body records what it made.
        `run_seed` recompiles these seeds within their bytecode's regime:
        below -O1 nothing is shared.

        A local initialized with a literal (`local tag = 7`) is left out
        while the decompiler loses its capture at -g2: Luau keeps the
        capture (NEWCLOSURE) but folds the constant into the closure body,
        and the printed `function() return 7 end` is shared once recompiled
        (pre-existing; M2 fuzz report, N1)."""
        rng = self.deinline_rng
        shape = rng.choice(IDENTITY_SHAPES)
        bind, made, kept, free = self.fresh("bind"), self.fresh("made"), self.fresh("kept"), self.fresh("free")
        captured = self.fresh("tag")
        initial = {"chunk": "tostring(#{1, 2, 3} + 7)", "computed": "tostring(7)", "function": None}[shape]
        declaration = f"local {captured} = {initial}" if initial else f"local function {captured}() return 3 end"
        loop = rng.choice(["for i = 1, 2 do", "for _, i in {1, 2} do" if shape == "chunk" else "for i in iterate(2) do"])
        returned = rng.choice(["5", "nil", '"free"'])
        compared = f"record({made}[1] == {made}[2], {kept}[1] == {kept}[2], {free}[1] == {free}[2], " \
                   f"{made}[1]() == {kept}[2]())"
        lines = [declaration, f"local function {bind}(p)", "    return function() return p end", "end", loop,
                 f"    {made}[i] = {bind}({captured})", f"    {kept}[i] = function() return {captured} end",
                 f"    {free}[i] = function() return {returned} end", "end"]
        if shape != "chunk":
            return [self.unit([f"local {made}, {kept}, {free} = {{}}, {{}}, {{}}", *lines, compared])]
        return [ChunkUnit([f"local {made}, {kept}, {free} = {{}}, {{}}, {{}}", *self.unit(lines)]),
                self.unit([compared])]

    def service_handle(self):
        """GetService and require handles passed to a helper -O2 inlines
        (D4: the source's handle declarations stay as they are). `game` and
        `require` are global stubs, so the program runs in the benchmark VM;
        both log their calls, so a handle call moved past other code shows."""
        rng = self.deinline_rng
        services, setup = self.fresh("services"), self.fresh("setup")
        returns = rng.random() < 0.5
        lines = [f"local {services} = {{Lighting = {{}}, Players = {{}}, Config = {{name = \"Config\"}}}}",
                 f"game = {{GetService = function(_, name) record(\"service\", name) return {services}[name] end}}",
                 "require = function(module) record(\"require\", module.name) return module end",
                 f"local function {setup}(s, n)", "    s.Brightness = n", "    s.ClockTime = n * 2",
                 *(["    return s"] if returns else []), "end"]

        def configure(handle, amount):
            if returns and rng.random() < 0.5:
                return f"local {self.fresh('configured')} = {setup}({handle}, {amount})"
            return f"{setup}({handle}, {amount})"

        for _ in range(rng.randint(1, 3)):
            amount = rng.choice(["input", "1", "flip and 2 or 3"])
            lighting = "local Lighting = game:GetService(\"Lighting\")"
            lines += {
                "declared": lambda: [lighting, configure("Lighting", amount)],
                "statement-between": lambda: [lighting, "record(\"between\")", configure("Lighting", amount)],
                "declaration-between": lambda: [lighting, f"local amount = record({amount})",
                                                configure("Lighting", "amount")],
                "argument": lambda: [configure("game:GetService(\"Players\")", amount)],
                "require": lambda: [f"local Config = require({services}.Config)", configure("Config", amount)],
                "twice": lambda: ["local Players = game:GetService(\"Players\")", configure("Players", "1"),
                                  configure("Players", amount)],
            }[rng.choice(HANDLE_SITES)]()
        lines.append(f"record({services}.Lighting, {services}.Players, {services}.Config)")
        return [self.unit(lines)]

    def constant_args(self):
        """Helpers -O2 inlines with constant arguments, which it folds into
        the copies: a pure helper's copy becomes one number
        (`frames(13)` as 0.21666666666666667), a statement helper's `1 - k`
        and `k * 10` numbers in its statements. Beside them, the same
        numbers by hand (a constant expression -O2 folds alike, in the
        caller's own code) and copies with a variable argument. The
        de-inliner may rebuild such a call only where Luau's folding rules
        prove it, and never through `^`."""
        rng = self.deinline_rng
        pure, put, sink = self.fresh("frames"), self.fresh("put"), self.fresh("sink")
        body = rng.choice(PURE_BODIES)
        lines = [f"local function {pure}(n) return {body} end",
                 f"local function {put}(t, k)", "    t.a = 1 - k", "    t.b = k * 10", "    record(t.a, t.b)", "end",
                 f"local {sink} = {{}}"]
        for _ in range(rng.randint(2, 5)):
            argument = rng.choice(CONSTANT_ARGUMENTS)
            lines += {
                "call": lambda: [f"record({pure}({argument}))"],
                "by-hand": lambda: [f"record({body.replace('n', f'({argument})')})"],
                "statement": lambda: [f"{put}({sink}, {argument})"],
                "statement-by-hand": lambda: [f"{sink}.a = 1 - {argument}", f"{sink}.b = {argument} * 10",
                                              f"record({sink}.a, {sink}.b)"],
            }[rng.choice(CONSTANT_SITES)]()
        return [self.unit(lines)]

    @staticmethod
    def unit(lines):
        # Its own block, so its locals end with it: the pressure unit after
        # it may hold close to the 200 locals Luau allows.
        return ["do", *("    " + line for line in lines), "end"]

    def program(self):
        units = []
        if self.has("deinline"):
            units.append(self.helper_definitions())
        self.locals.append([])
        for _ in range(self.rng.randint(4, 10)):
            units.append(self.statement(3))
        tail = []
        if self.has("deinline"):
            tail.append(self.inline_copies())
        if self.has("pressure"):
            tail.append(self.pressure())
        self.locals.pop()
        units += self.capture_units() + self.deinline_units() + tail
        # Units run in `body`, one level in; a `ChunkUnit` runs in the main
        # chunk and stays unindented (see `source_of`).
        return ["".join(("" if isinstance(unit, ChunkUnit) else "    ") + line + "\n" for line in unit)
                for unit in units]


def generate(seed):
    generator = Generator(seed)
    return generator.program(), sorted(generator.families)


def source_of(units):
    """The program: main-chunk units (unindented) go right before `body`,
    the others into it. Without main-chunk units, the prelude, the units
    and the ending, as before there were any."""
    split = PRELUDE.index("local function body(")
    chunk = "".join(unit for unit in units if not unit.startswith(" "))
    inner = "".join(unit for unit in units if unit.startswith(" "))
    return PRELUDE[:split] + chunk + PRELUDE[split:] + inner + ENDING


def run(command, timeout):
    try:
        process = subprocess.run([str(part) for part in command], capture_output=True, timeout=timeout)
        return process.returncode, process.stdout.decode("utf-8", "replace").replace("\r\n", "\n"), \
            process.stderr.decode("utf-8", "replace").replace("\r\n", "\n")
    except subprocess.TimeoutExpired:
        return "timeout", "", ""


def compile_luau(args, path, opt, debug):
    process = subprocess.run([str(args.compiler), "--binary", "--fflags=false", f"-O{opt}", f"-g{debug}",
                              "--vector-lib=vector", "--vector-ctor=create", str(path)], capture_output=True,
                             timeout=args.timeout)
    if process.returncode:
        raise CompileError(process.stderr.decode("utf-8", "replace")[:500])
    return process.stdout


class CompileError(Exception):
    pass


def mutate(rng, data):
    """A patch only a hand-made chunk has: a string renamed to one no
    identifier spells, or a number constant turned into a NaN payload."""
    strings = sorted(set(re.findall(rb"[A-Za-z_][A-Za-z0-9_]{2,}", data)))
    if strings and rng.random() < 0.7:
        name = rng.choice(strings)
        position = rng.randrange(1, len(name))
        patched = name[:position] + b" " + name[position + 1:]
        return data.replace(name, patched, 1), f"rename {name.decode()} -> {patched.decode()}"
    number = struct.pack("<d", 0.5)
    if number in data:
        return data.replace(number, struct.pack("<Q", 0x7ff8000000001234), 1), "nan payload"
    return data, "none"


def check(args, units, directory, opt, debug, out_opt, mutation=None):
    """One profile. Returns (status, detail)."""
    directory.mkdir(parents=True, exist_ok=True)
    source = directory / "source.luau"
    source.write_text(source_of(units), encoding="utf-8", newline="\n")
    try:
        data = compile_luau(args, source, opt, debug)
    except CompileError as error:
        return "invalid", f"source compile: {error}"
    note = None
    if mutation is not None:
        data, note = mutate(random.Random(mutation), data)
    (directory / "input.bc").write_bytes(data)
    reference = run([args.vm, directory / "input.bc", args.driver_bc], args.timeout)
    if reference[0] != 0:
        return "invalid", f"reference exit {reference[0]}: {reference[2][:300]}"
    # Where memory runs out depends on how much each version allocates.
    if "not enough memory" in reference[1]:
        return "invalid", "reference ran out of memory"
    code, output, error = run([args.lifter, directory / "input.bc"], args.timeout)
    if code == "timeout":
        return "failed", "decompile timeout"
    if code != 0:
        text = output + error
        if "panicked" in text:
            return "failed", f"panic: {text.strip()[-300:]}"
        if any(reason in text for reason in REFUSALS):
            return "refused", text.strip()[-200:]
        return "failed", f"decompile error: {text.strip()[-300:]}"
    emitted = directory / "output.luau"
    emitted.write_text(output, encoding="utf-8", newline="\n")
    try:
        rebuilt = compile_luau(args, emitted, out_opt, debug)
    except CompileError as error:
        return "failed", f"recompile -O{out_opt}: {error}"
    (directory / "output.bc").write_bytes(rebuilt)
    actual = run([args.vm, directory / "output.bc", args.driver_bc], args.timeout)
    expected_output, actual_output = comparable(reference[1]), comparable(actual[1])
    if actual[0] != reference[0] or actual_output != expected_output:
        detail = first_difference(expected_output, actual_output) if actual[0] == reference[0] else f"exit {actual[0]}: {actual[2][:200]}"
        return "failed", f"mismatch (output -O{out_opt}{', ' + note if note else ''}): {detail}"
    return "passed", note


def first_difference(expected, actual):
    for index, (left, right) in enumerate(zip(expected.splitlines(), actual.splitlines())):
        if left != right:
            return f"line {index + 1}: expected {left[:200]!r} got {right[:200]!r}"
    return "different line count"


def reduce(args, units, directory, profile, category):
    """Delete whole units while the failure keeps its category."""
    result, attempts, index = list(units), 0, 0
    while index < len(result) and attempts < args.reduce_budget:
        candidate = result[:index] + result[index + 1:]
        attempts += 1
        status, detail = check(args, candidate, directory / f"reduce{attempts}", *profile)
        shutil.rmtree(directory / f"reduce{attempts}", ignore_errors=True)
        if status == "failed" and detail.split(":")[0] == category:
            result, index = candidate, 0
        else:
            index += 1
    (directory / "reduced.luau").write_text(source_of(result), encoding="utf-8", newline="\n")
    return attempts, len(units), len(result)


def run_seed(args, seed):
    rng = random.Random(seed * 7919 + 1)
    units, families = generate(seed)
    directory = args.work / f"seed{seed}"
    rows = []
    profiles = rng.sample([(opt, debug) for opt in (0, 1, 2) for debug in (0, 1, 2)], args.profiles)
    for opt, debug in profiles:
        # Luau's -O2 inliner changes what code observes (it removes the call
        # frames `debug.info` reads and reads a caller's register where the
        # callee read an upvalue), so output of bytecode built below -O2 is
        # not compiled again at -O2.
        out_opts = (0, 1, 2) if opt == 2 else (0, 1)
        out_opt = rng.choice(out_opts)
        if "closure-identity" in families and (out_opt == 0) != (opt == 0):
            # Luau shares closures (DUPCLOSURE) only from -O1: identity
            # survives a recompile only within the regime of the bytecode.
            out_opt = opt
        profile = (opt, debug, out_opt, seed if args.mutate and rng.random() < 0.5 else None)
        case = directory / f"O{opt}g{debug}"
        status, detail = check(args, units, case, *profile)
        if status == "failed":
            # A crash of the VM under load (stack overflow) does not repeat.
            status, detail = check(args, units, case, *profile)
        row = {"seed": seed, "families": families, "opt": opt, "debug": debug, "out_opt": profile[2],
               "mutation": profile[3] is not None, "status": status, "detail": detail}
        if status == "failed" and args.reduce:
            row["reducer"] = reduce(args, units, case, profile, detail.split(":")[0])
        elif status != "failed":
            shutil.rmtree(case, ignore_errors=True)
        rows.append(row)
    if all(row["status"] != "failed" for row in rows):
        shutil.rmtree(directory, ignore_errors=True)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("compiler", "vm", "lifter"):
        parser.add_argument(f"--{name}", type=pathlib.Path, required=True)
    parser.add_argument("--seed-start", type=int, default=0)
    parser.add_argument("--seeds", type=int, default=50)
    parser.add_argument("--profiles", type=int, default=3, help="optimization/debug profiles per seed (of 9)")
    parser.add_argument("--mutate", action="store_true", help="also patch the bytecode of half the profiles")
    parser.add_argument("--workers", type=int, default=os.cpu_count())
    parser.add_argument("--timeout", type=float, default=20)
    parser.add_argument("--reduce", action="store_true")
    parser.add_argument("--reduce-budget", type=int, default=60)
    parser.add_argument("--work", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path, required=True)
    args = parser.parse_args()
    for name in ("compiler", "vm", "lifter"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    args.work = args.work.resolve()
    args.work.mkdir(parents=True, exist_ok=True)
    driver = args.work / "driver.luau"
    driver.write_text(DRIVER, encoding="utf-8", newline="\n")
    args.driver_bc = args.work / "driver.bc"
    args.driver_bc.write_bytes(compile_luau(args, driver, 1, 1))
    rows = []
    seeds = range(args.seed_start, args.seed_start + args.seeds)
    with concurrent.futures.ProcessPoolExecutor(max_workers=args.workers) as pool:
        for seed_rows in pool.map(run_seed, [args] * len(seeds), seeds):
            rows += seed_rows
            for row in seed_rows:
                if row["status"] == "failed":
                    print(f"seed {row['seed']} O{row['opt']}g{row['debug']} {row['families']}: {row['detail'][:300]}",
                          flush=True)
    summary = dict(collections.Counter(row["status"] for row in rows))
    # A failing row counts for every family its program drew: a hint for
    # triage, not a cause (the reduced program shows which unit it needs).
    by_family = collections.defaultdict(collections.Counter)
    for row in rows:
        for family in row["families"]:
            by_family[family][row["status"]] += 1
    report = {"schema_version": 1, "generator": VERSION, "summary": summary,
              "by_family": {family: dict(counts) for family, counts in sorted(by_family.items())}, "rows": rows,
              "limitations": "Finite typed grammar and inputs; no exhaustive equivalence claim. "
                             "The reducer keeps the failure category, not necessarily the root cause."}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(summary))
    return int(summary.get("failed", 0) > 0)


if __name__ == "__main__":
    sys.exit(main())
