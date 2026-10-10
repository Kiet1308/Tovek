import collections
import pathlib
import random
import re
import shutil
import struct
import tempfile
import unittest
from unittest import mock

import fuzz_roundtrip
from fuzz_roundtrip import (CAPTURE_FAMILIES, CAPTURE_SHAPES, CELL_HOSTS, CLOSURE_BINDERS, CONSTANT_SITES, DEINLINE_FAMILIES, ENDING,
                            ERROR_LEVELS, FAMILIES, HANDLE_SITES, IDENTITY_SHAPES, PRELUDE, PURE_BODIES, RECURSIVE_ARMS,
                            WRITTEN_PARAM_SITES, ChunkUnit, Generator, comparable, generate, mutate, source_of)


def text_of(units):
    return "\n".join("\n".join(unit) for unit in units)


class FuzzRoundtripTests(unittest.TestCase):
    def test_seeds_replay_and_explore(self):
        self.assertEqual(generate(7), generate(7))
        programs = {tuple(generate(seed)[0]) for seed in range(20)}
        self.assertEqual(len(programs), 20)

    def test_families_come_from_the_known_set(self):
        for seed in range(50):
            _, families = generate(seed)
            self.assertTrue(set(families) <= set(FAMILIES) | set(CAPTURE_FAMILIES) | set(DEINLINE_FAMILIES))
            self.assertTrue(1 <= len(set(families) & set(FAMILIES)) <= 4)

    def test_source_wraps_units_in_a_returned_function(self):
        units, _ = generate(3)
        source = source_of(units)
        self.assertTrue(source.rstrip().endswith("end"))
        self.assertIn("return function(input, flip)", source)
        for unit in units:
            self.assertIn(unit, source)

    def test_capture_families_leave_every_other_unit_as_it_was(self):
        # The capture families draw from their own stream: without them a
        # seed builds the units it built before they existed, in order. (The
        # de-inline families come after them and take later names.)
        drawn = set()
        for seed in range(60):
            with mock.patch.object(Generator, "deinline_units", lambda self: []):
                units, families = generate(seed)
                with mock.patch.object(Generator, "capture_units", lambda self: []):
                    plain, plain_families = generate(seed)
            captures = set(families) & set(CAPTURE_FAMILIES)
            drawn |= captures
            self.assertEqual(set(families) - captures, set(plain_families))
            self.assertEqual([unit for unit in units if unit in plain], plain)
            self.assertEqual(len(units) - len(plain), len(captures))
        self.assertEqual(drawn, set(CAPTURE_FAMILIES))

    def test_deinline_families_leave_every_other_unit_as_it_was(self):
        # A third stream: without them a seed builds the units it built
        # before they existed (the capture families' included), in order.
        drawn = set()
        for seed in range(80):
            units, families = generate(seed)
            with mock.patch.object(Generator, "deinline_units", lambda self: []):
                plain, plain_families = generate(seed)
            added = set(families) & set(DEINLINE_FAMILIES)
            drawn |= added
            self.assertEqual(set(families) - added, set(plain_families))
            self.assertEqual([unit for unit in units if unit in plain], plain)
            if not added:
                self.assertEqual(source_of(units), source_of(plain))
        self.assertEqual(drawn, set(DEINLINE_FAMILIES))

    def test_family_units_come_before_the_pressure_unit(self):
        # Their locals must not stack on top of the pressure unit's ~185:
        # each is a block of its own, or runs in the main chunk.
        seen = set()
        for seed in range(600):
            units, families = generate(seed)
            extra = set(families) & (set(CAPTURE_FAMILIES) | set(DEINLINE_FAMILIES))
            if "pressure" not in families or not extra:
                continue
            seen |= extra
            self.assertTrue(units[-1].lstrip().startswith("local r"), seed)
            # After them come the inline copies of the `deinline` family.
            tail = 2 if "deinline" in families else 1
            chunk = sum(1 for unit in units if not unit.startswith(" "))
            between = units[-tail - len(extra) - chunk:-tail]
            self.assertEqual(sum(1 for unit in between if not unit.startswith(" ")), chunk)
            self.assertTrue(all(unit.startswith(("    do\n", "local ")) for unit in between), seed)
        self.assertEqual(seen, set(CAPTURE_FAMILIES) | set(DEINLINE_FAMILIES))

    def test_main_chunk_units_go_before_body(self):
        inner, chunk = "    record(1)\n", "local kept = {}\n"
        self.assertEqual(source_of([inner]), PRELUDE + inner + ENDING)
        source = source_of([chunk, inner])
        self.assertLess(source.index(chunk), source.index("local function body("))
        self.assertLess(source.index("local function body("), source.index(inner))
        self.assertEqual(source.replace(chunk, "", 1), PRELUDE + inner + ENDING)

    def test_closure_binders_reach_the_assignment_three_ways(self):
        for shape in CAPTURE_SHAPES:
            for binder in CLOSURE_BINDERS:
                with self.subTest(shape=shape, binder=binder):
                    source = "\n".join(Generator(1).capture_factory(shape, binder))
                    self.assertTrue(source.startswith("do\n") and source.endswith("\nend"))
                    if binder == "factory":
                        # The closure is made inside the factory, never as a literal at the site.
                        self.assertRegex(source, r"local function make\d+\(")
                        self.assertNotRegex(source, r"\((?:keep\d+|record)\(function")
                    elif binder == "keep":
                        self.assertRegex(source, r"keep\d+\(function\(")
                    else:
                        self.assertRegex(source, r"record\(function\(")

    def test_shapes_keep_their_variables_out_of_reach_of_inlining(self):
        # The vulnerable code must run as its own prototype: the branch
        # shapes return the function that holds it, the others are reached
        # through `record`, so no inlined copy folds a constant argument.
        generator = Generator(2)
        self.assertRegex("\n".join(generator.capture_factory("loop", "record")),
                         r"local previous = acc\n\s+acc = record\(function\(x\) return previous\(x\) \+ i end\)")
        self.assertRegex("\n".join(generator.capture_factory("loop", "factory")), r"acc = make\d+\(acc, i\)")
        for shape in ("loop", "parallel"):
            self.assertRegex("\n".join(generator.capture_factory(shape, "keep")), r"= record\((chain|pair)\d+\)\(")
        self.assertRegex("\n".join(generator.capture_factory("branch", "keep")), r"record\(pick\d+\(if flip then")

    def test_recursive_arms(self):
        for arm in RECURSIVE_ARMS:
            for indirect in (False, True):
                with self.subTest(arm=arm, indirect=indirect):
                    source = "\n".join(Generator(3).recursive_arm(arm, indirect))
                    rec = re.search(r"local function (rec\d+)\(n\)", source).group(1)
                    self.assertIn(f"return {rec}(n - 1)", source)
                    self.assertIn(f"chosen = {rec}", source)
                    self.assertIn("if not flag then" if arm == "else" else "if flag then", source)
                    self.assertEqual("else\n" in source, arm.startswith("diamond"))
                    self.assertEqual(bool(re.search(r"= record\(choose\d+\)\(flip", source)), indirect)

    def test_written_param_copies_write_a_caller_local(self):
        # The helper writes its parameter under a test; a copy writes the
        # caller's local the same way (outside the helper, so the write
        # appears more than once); every host function is reached through
        # `record`, and each site kind turns up.
        sites = collections.Counter()
        for seed in range(150):
            source = text_of(Generator(seed).written_param())
            helper = re.search(r"local function (put\d+)\((at, )?v\)\n\s+(if v (>|<|~=) .* end)\n", source)
            self.assertIsNotNone(helper, source)
            name, write = helper.group(1), helper.group(3)
            self.assertRegex(source, rf"\n\s+((local )?s = )?{name}\((8, )?input\)\n")
            hosts = re.findall(r"local function (host\d+)\(", source)
            self.assertTrue(hosts)
            for host in hosts:
                self.assertIn(f"record(record({host})(", source)
            self.assertTrue(source.endswith(f"    record({re.search(r'local (out\d+) = ', source).group(1)})\nend"))
            # A host function's body is indented by 8, the helper's site by 8 or 12.
            for kind, pattern in (("after-if", r"\n        if go then\n(?:            [^\n]*\n)+        end\n"
                                               r"        return v"),
                                  ("loop", r"for i (= 1, count|in iterate\(count\)) do\n"),
                                  ("while", r"while v (>|<|~=) [v0-9]+ and n < 3 do\n"),
                                  ("same-block", r"host\d+\(v\)\n(?:        [^\n]*\n)+?        return v(, s)?\n"),
                                  ("dead", r"local w = v \+ k\n(?:        [^\n]*\n)*?        return out\d+\[7\]\n")):
                sites[kind] += bool(re.search(pattern, source))
            copies = [line for line in source.splitlines() if re.fullmatch(r"\s+if [vw] (>|<|~=) .* end", line)]
            sites["copy"] += len(copies) > 1
            sites["call"] += bool(re.search(rf"{name}\(([a-z]+, )?[vw]\)", source))
            self.assertTrue(all(line.strip().replace("w", "v") == write for line in copies))
        self.assertEqual(set(sites), set(WRITTEN_PARAM_SITES) | {"copy", "call"})
        self.assertTrue(all(sites.values()), sites)

    def test_returned_cell_returns_one_outer_local_on_every_path(self):
        hosts = collections.Counter()
        for seed in range(150):
            source = text_of(Generator(seed).returned_cell())
            cell, tag = re.search(r"local (list\d+), (tag\d+), count\d+ = \{\}, \"old\", 0", source).groups()
            helper = re.search(r"local function (refill\d+)\(x, y\)\n((.*\n)+?)    end", source)
            body = helper.group(2)
            self.assertEqual(set(re.findall(r"return (\S+)", body)), {cell})
            self.assertRegex(body, rf"{tag} = ")
            hosts["tag-first"] += bool(re.search(rf"record\({tag}, l\d+\[1\], l\d+\[2\]\)", source))
            hosts["count-first"] += bool(re.search(r"record\(count\d+, l\d+\[1\]\)", source))
            hosts["cell-first"] += bool(re.search(rf"record\(l\d+\[1\], {tag}, count\d+\)", source))
            hosts["concat"] += bool(re.search(rf"local joined = {tag} \.\. ", source))
            hosts["constructor"] += bool(re.search(rf"record\(\{{{tag}, l\d+\[2\], count\d+\}}\)", source))
            hosts["length"] += bool(re.search(rf"record\(#l\d+, {tag}\)", source))
            hosts["direct"] += bool(re.search(rf"record\({tag}, {helper.group(1)}\(x, \w+\)\[1\]\)", source))
            hosts["block"] += bool(re.search(r"\n\s+do\n\s+local x, y = ", source))
            hosts["inlined"] += bool(re.search(r"\n\s+show\d+\(", source))
            hosts["reached"] += bool(re.search(r"record\(show\d+\)\(", source))
        self.assertEqual(set(hosts), set(CELL_HOSTS) | {"block", "inlined", "reached"})
        self.assertTrue(all(hosts.values()), hosts)

    def test_error_levels_are_recorded_by_whether_a_position_is_there(self):
        levels = set()
        for seed in range(100):
            source = text_of(Generator(seed).error_level())
            level = re.search(r"error\((\"bad \" \.\. name|\{reason = name\})(.*?)\) end", source)
            self.assertIsNotNone(level, source)
            levels.add(level.group(2))
            self.assertRegex(source, r"local ok\d+, problem\d+ = pcall\((set\d+, |function\(\) check\d+\()")
            self.assertIn('(string.gsub(problem', source)
            self.assertIn('"^[^:]*:%d+: ", "@:"))', source)
        self.assertEqual(levels, set(ERROR_LEVELS))

    def test_closure_identity_compares_closures_made_in_a_loop(self):
        shapes = set()
        for seed in range(100):
            units = Generator(seed).closure_identity()
            source = text_of(units)
            self.assertRegex(source, r"record\(made\d+\[1\] == made\d+\[2\], kept\d+\[1\] == kept\d+\[2\], "
                                     r"free\d+\[1\] == free\d+\[2\], made\d+\[1\]\(\) == kept\d+\[2\]\(\)\)")
            self.assertRegex(source, r"made\d+\[i\] = bind\d+\(tag\d+\)\n\s+"
                                     r"kept\d+\[i\] = function\(\) return tag\d+ end\n\s+"
                                     r"free\d+\[i\] = function\(\) return (5|nil|\"free\") end")
            if len(units) == 2:
                # The main chunk makes the closures (its local is a register
                # there); the body only compares them.
                shapes.add("chunk")
                self.assertIsInstance(units[0], ChunkUnit)
                self.assertNotIsInstance(units[1], ChunkUnit)
                self.assertNotIn("iterate(", "\n".join(units[0]))
                self.assertEqual(len(units[1]), 3)
            else:
                initial = re.search(r"local (function )?tag\d+( = (.*))?", source)
                shapes.add("function" if initial.group(1) else initial.group(3))
        # A literal constant too: with -g2 Luau keeps its capture but folds
        # its reads (N1).
        self.assertEqual(shapes, {"chunk", "function", "tostring(7)", "7", "\"k\"", "true"})
        self.assertEqual(set(IDENTITY_SHAPES), {"chunk", "constant", "function", "computed"})

    def test_main_chunk_units_stay_unindented(self):
        for seed in range(300):
            units, families = generate(seed)
            chunk = [unit for unit in units if not unit.startswith(" ")]
            if chunk:
                self.assertIn("closure-identity", families)
                self.assertTrue(all(unit.startswith("local made") for unit in chunk))
                source = source_of(units)
                self.assertLess(source.index(chunk[0]), source.index("local function body("))
                return
        self.fail("no seed draws the main-chunk shape")

    def test_closure_identity_recompiles_within_the_sharing_regime(self):
        # DUPCLOSURE shares only from -O1: such a seed's output is compiled
        # again on the same side of -O1 as its bytecode, and every other
        # seed keeps the profiles it had.
        work = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, work, ignore_errors=True)

        def profiles(seed):
            calls = []
            args = mock.Mock(work=work, profiles=9, mutate=True, reduce=False)
            with mock.patch.object(fuzz_roundtrip, "check",
                                   lambda args, units, case, *profile: calls.append(profile) or ("passed", None)), \
                    mock.patch.object(fuzz_roundtrip, "source_differs", lambda *_: False):
                fuzz_roundtrip.run_seed(args, seed)
            return calls

        seeds = {seed: "closure-identity" in generate(seed)[1] for seed in range(120)}
        for seed in [seed for seed, identity in seeds.items() if identity][:8]:
            for opt, _, out_opt, _ in profiles(seed):
                self.assertEqual(opt == 0, out_opt == 0, seed)
        crossing = 0
        for seed in [seed for seed, identity in seeds.items() if not identity][:16]:
            calls = profiles(seed)
            crossing += any((opt == 0) != (out_opt == 0) for opt, _, out_opt, _ in calls)
            # The profiles come from the seed alone, as before the family existed.
            with mock.patch.object(fuzz_roundtrip, "generate", lambda seed: ([], [])):
                self.assertEqual(profiles(seed), calls)
        self.assertGreater(crossing, 0)

    def test_level_dependent_sources_recompile_minus_o0_bytecode_at_minus_o0(self):
        # Where the program itself prints differently at -O0 and -O1, -O0
        # bytecode is compiled again at -O0, and its row says so. Only -O0
        # bytecode drawn another level asks; nothing else changes.
        work = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, work, ignore_errors=True)

        def run(seed, differs):
            calls, asked = [], []
            args = mock.Mock(work=work, profiles=9, mutate=True, reduce=False)

            def source_differs(args, units, case, out_opt, debug):
                asked.append((case.name, out_opt))
                return differs
            with mock.patch.object(fuzz_roundtrip, "check",
                                   lambda args, units, case, *profile: calls.append(profile) or ("passed", None)), \
                    mock.patch.object(fuzz_roundtrip, "source_differs", source_differs):
                rows = fuzz_roundtrip.run_seed(args, seed)
            return calls, asked, rows

        seed = next(seed for seed in range(200) if "closure-identity" not in generate(seed)[1]
                    and any(opt == 0 and out_opt != 0 for opt, _, out_opt, _ in run(seed, False)[0]))
        same, asked, rows = run(seed, False)
        self.assertTrue(asked and all(name.startswith("O0") and out_opt != 0 for name, out_opt in asked))
        self.assertFalse(any("source_differs" in row for row in rows))
        clamped, _, rows = run(seed, True)
        for before, after, row in zip(same, clamped, rows):
            expected = (before[0], before[1], 0, before[3]) if before[0] == 0 else before
            self.assertEqual(after, expected)
            self.assertEqual(row.get("source_differs", False), before[0] == 0 and before[2] != 0)

    def test_source_differs_compares_the_two_levels(self):
        work = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, work, ignore_errors=True)
        args = mock.Mock(timeout=5)
        outputs = {}
        with mock.patch.object(fuzz_roundtrip, "compile_luau", lambda args, path, opt, debug: bytes([opt])), \
                mock.patch.object(fuzz_roundtrip, "run",
                                  lambda command, timeout: (0, outputs[pathlib.Path(command[1]).read_bytes()[0]], "")):
            outputs.update({0: "1:6,6 table: 0x1", 1: "1:6,6 table: 0x2"})
            self.assertFalse(fuzz_roundtrip.source_differs(args, [], work / "a", 1, 1))
            outputs[1] = "1:6,6.5 table: 0x2"
            self.assertTrue(fuzz_roundtrip.source_differs(args, [], work / "b", 1, 1))

    def test_service_handles_use_global_stubs(self):
        sites = set()
        for seed in range(100):
            source = text_of(Generator(seed).service_handle())
            self.assertRegex(source, r"\n    game = \{GetService = function\(_, name\) record\(\"service\", name\)")
            self.assertIn('\n    require = function(module) record("require", module.name) return module end', source)
            setup = re.search(r"local function (setup\d+)\(s, n\)", source).group(1)
            for site, pattern in (
                    ("declared", r"GetService\(\"Lighting\"\)\n\s+(local \w+ = )?{0}\(Lighting"),
                    ("statement-between", r"GetService\(\"Lighting\"\)\n\s+record\(\"between\"\)\n\s+(local \w+ = )?{0}\("),
                    ("declaration-between", r"GetService\(\"Lighting\"\)\n\s+local amount = record\("),
                    ("argument", r"{0}\(game:GetService\(\"Players\"\)"),
                    ("require", r"local Config = require\(services\d+\.Config\)\n\s+(local \w+ = )?{0}\(Config"),
                    ("twice", r"{0}\(Players, 1\)\n\s+(local \w+ = )?{0}\(Players")):
                if re.search(pattern.format(setup), source):
                    sites.add(site)
        self.assertEqual(sites, set(HANDLE_SITES))

    def test_constant_arguments_meet_numbers_by_hand(self):
        sites, bodies = set(), set()
        for seed in range(150):
            source = text_of(Generator(seed).constant_args())
            pure = re.search(r"local function (frames\d+)\(n\) return (.+) end", source)
            put = re.search(r"local function (put\d+)\(t, k\)\n\s+t\.a = 1 - k\n\s+t\.b = k \* 10", source)
            self.assertTrue(pure and put, seed)
            bodies.add(pure.group(2))
            for site, pattern in (
                    ("call", r"record\({0}\("),
                    ("statement", r"\n\s+{1}\(sink\d+, "),
                    ("statement-by-hand", r"sink\d+\.a = 1 - "),
                    ("by-hand", r"record\(.*\((?:-?[\d.e]+|input)\)")):
                if re.search(pattern.format(pure.group(1), put.group(1)), source):
                    sites.add(site)
        self.assertEqual(sites, set(CONSTANT_SITES))
        self.assertEqual(bodies, set(PURE_BODIES))

    def test_mutations_keep_the_length_and_spell_no_identifier(self):
        data = b"\x06\x03\x02\x06record\x04next" + struct.pack("<d", 0.5)
        for seed in range(20):
            patched, note = mutate(random.Random(seed), data)
            self.assertEqual(len(patched), len(data))
            if note.startswith("rename"):
                self.assertIn(b" ", patched)
            elif note == "nan payload":
                self.assertNotIn(struct.pack("<d", 0.5), patched)

    def test_heap_addresses_compare_as_one_token(self):
        # Seed 142948 with "function" renamed: `describe` printed each function
        # by address, which differs between runs and across DUPCLOSURE levels.
        reference = '1:function: 0x0000023b3b4aaef0;1:"fallback" => true,-3'
        rebuilt = '1:function: 0x00000194410ead10;1:"fallback" => true,-3'
        self.assertEqual(comparable(reference), comparable(rebuilt))
        self.assertEqual(comparable("t: table: 0xAB12"), "t: table: 0x")
        # Everything else still differs.
        self.assertNotEqual(comparable("1:function: 0x1;1:2"), comparable("1:function: 0x1;1:3"))
        self.assertEqual(comparable('1:"0x10",16'), '1:"0x10",16')


if __name__ == "__main__":
    unittest.main()
