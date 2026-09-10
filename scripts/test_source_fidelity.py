import copy
import unittest
from unittest.mock import patch
from subprocess import CompletedProcess

from source_fidelity import canonicalize, compare_ast, parse_ast


def binding(name, location):
    return {"type": "AstLocal", "name": name, "location": location, "luauType": None}


def program(which=0, names=("left", "right")):
    args = [binding(n, str(i)) for i, n in enumerate(names)]
    return {"type": "AstExprFunction", "args": args, "body": {"type": "AstStatBlock", "body": [
        {"type": "AstStatReturn", "list": [{"type": "AstExprLocal", "local": args[which]}]}]}}


class SourceFidelityTests(unittest.TestCase):
    def test_ast_cli_non_utf8_constant_bytes_stay_distinct(self):
        def parse(value):
            result = CompletedProcess([], 0, b'{"root":{"type":"AstExprConstantString","value":"' + bytes([value]) + b'"}}')
            with patch("source_fidelity.subprocess.run", return_value=result):
                return parse_ast("luau-ast", "input.luau")
        self.assertEqual(compare_ast(parse(255), parse(255))["raw_structural_ratio"], 1)
        self.assertLess(compare_ast(parse(255), parse(254))["raw_structural_ratio"], 1)

    def test_alpha_rename_preserves_structure_but_not_exact_name_score(self):
        metric = compare_ast(program(), program(names=("p", "p2")))
        self.assertEqual(metric["raw_structural_ratio"], 1)
        self.assertEqual(metric["aligned_bindings"], 2)
        self.assertEqual(metric["exact_names"], 0)

    def test_wrong_binding_is_not_erased(self):
        metric = compare_ast(program(0), program(1))
        self.assertLess(metric["raw_structural_ratio"], 1)
        self.assertEqual(metric["aligned_bindings"], 0)

    def test_same_spelling_different_scope_keeps_distinct_identity(self):
        tree = program(names=("value", "value"))
        normalized, names, _ = canonicalize(tree)
        self.assertEqual(len(names), 2)
        self.assertEqual(normalized["args"][0]["id"], 0)
        self.assertEqual(normalized["args"][1]["id"], 1)

    def test_type_and_trivia_separate_from_structure(self):
        tree = copy.deepcopy(program())
        tree["args"][0]["luauType"] = {"type": "AstTypeReference", "name": "number"}
        tree["location"] = "50 - 99"
        metric = compare_ast(tree, program())
        self.assertEqual(metric["raw_structural_ratio"], 1)
        self.assertFalse(metric["type_syntax_equal"])

    def test_grouped_call_and_global_names_remain_significant(self):
        call = {"type": "AstExprCall", "func": {"type": "AstExprGlobal", "global": "f"}, "args": []}
        grouped = {"type": "AstExprGroup", "expr": call}
        self.assertLess(compare_ast(call, grouped)["raw_structural_ratio"], 1)
        other = copy.deepcopy(call)
        other["func"]["global"] = "g"
        self.assertLess(compare_ast(call, other)["raw_structural_ratio"], 1)

    def test_statement_style_normalization_is_explicit_and_limited(self):
        local = binding("selected", "0")
        condition = {"type": "AstExprGlobal", "global": "condition"}
        yes, no = {"type": "AstExprConstantBool", "value": False}, {"type": "AstExprConstantNil"}
        source = {"type": "AstStatBlock", "body": [{"type": "AstStatLocal", "vars": [local],
            "values": [{"type": "AstExprIfElse", "condition": condition, "trueExpr": yes, "falseExpr": no}]}]}
        output = {"type": "AstStatBlock", "body": [
            {"type": "AstStatLocal", "vars": [local], "values": []},
            {"type": "AstStatIf", "condition": condition,
             "thenbody": {"type": "AstStatBlock", "body": [{"type": "AstStatAssign",
                 "vars": [{"type": "AstExprLocal", "local": local}], "values": [yes]}]},
             "elsebody": {"type": "AstStatBlock", "body": [{"type": "AstStatAssign",
                 "vars": [{"type": "AstExprLocal", "local": local}], "values": [no]}]}}]}
        metric = compare_ast(source, output)
        self.assertLess(metric["raw_structural_ratio"], 1)
        self.assertEqual(metric["statement_initializer_normalized_ratio"], 1)
        self.assertEqual(metric["output_conditional_expressions"], 0)

    def test_budget_refuses_instead_of_scoring_unknown_as_equal(self):
        self.assertEqual(compare_ast(program(), program(), token_pair_budget=1)["status"], "unknown")


if __name__ == "__main__":
    unittest.main()
