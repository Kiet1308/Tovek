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

VERSION = "typed-families-v1"

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

REFUSALS = ("headroom for the vector constructor", "a method name no identifier spells",
            "a NaN constant whose payload", "a global name no identifier spells",
            "more locals at once than Luau allows", "more registers than Luau allows",
            "no faithful source", "a builtin call names another function", "a loop prepared for")


class Generator:
    def __init__(self, seed):
        self.rng = random.Random(seed)
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

    def program(self):
        units = []
        if self.has("deinline"):
            units.append(self.helper_definitions())
        self.locals.append([])
        for _ in range(self.rng.randint(4, 10)):
            units.append(self.statement(3))
        if self.has("deinline"):
            units.append(self.inline_copies())
        if self.has("pressure"):
            units.append(self.pressure())
        self.locals.pop()
        return ["\n".join("    " + line for line in unit) + "\n" for unit in units]


def generate(seed):
    generator = Generator(seed)
    return generator.program(), sorted(generator.families)


def source_of(units):
    return PRELUDE + "".join(units) + ENDING


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
    if actual[:2] != reference[:2]:
        detail = first_difference(reference[1], actual[1]) if actual[0] == reference[0] else f"exit {actual[0]}: {actual[2][:200]}"
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
        profile = (opt, debug, rng.choice(out_opts), seed if args.mutate and rng.random() < 0.5 else None)
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
    report = {"schema_version": 1, "generator": VERSION, "summary": summary, "rows": rows,
              "limitations": "Finite typed grammar and inputs; no exhaustive equivalence claim. "
                             "The reducer keeps the failure category, not necessarily the root cause."}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(summary))
    return int(summary.get("failed", 0) > 0)


if __name__ == "__main__":
    sys.exit(main())
