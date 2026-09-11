import copy
import unittest
from types import SimpleNamespace

from bytecode_roundtrip import OPCODES
from capture_effects_audit import verify


def instruction(pc, name, a=0, b=0, d=0):
    return pc, OPCODES.index(name), a, b, 0, d, 0, 0


def fixture():
    prototypes = [
        SimpleNamespace(id=0, num_upvalues=0, max_stack=3, children=[1], constants=[], code=[0, 0],
                        insns=[instruction(0, "NEWCLOSURE"), instruction(1, "CAPTURE", a=0)]),
        SimpleNamespace(id=1, num_upvalues=1, max_stack=3, children=[2], constants=[], code=[0, 0],
                        insns=[instruction(0, "NEWCLOSURE"), instruction(1, "CAPTURE", a=2)]),
        SimpleNamespace(id=2, num_upvalues=1, max_stack=3, children=[], constants=[], code=[0],
                        insns=[instruction(0, "GETUPVAL")]),
    ]
    proof = dict(schema_version=1, model="luau-v9-val-upval-immutability-v1", status="complete",
                 refusal=None, analyzed_slots=2,
                 readonly_slots=[dict(prototype=1, slots=[0]), dict(prototype=2, slots=[0])])
    return proof, SimpleNamespace(version=9, main=0, protos=prototypes)


class CaptureEffectsAuditTests(unittest.TestCase):
    def test_valid_copied_chain_and_conservative_subset(self):
        proof, chunk = fixture()
        self.assertEqual(verify(proof, chunk)["readonly_slots"], 2)
        proof["readonly_slots"].pop()
        self.assertEqual(verify(proof, chunk)["readonly_slots"], 1)

    def test_ref_constructor_invalidates_existing_claim(self):
        proof, chunk = fixture()
        chunk.protos[0].insns[1] = instruction(1, "CAPTURE", a=1)
        with self.assertRaisesRegex(ValueError, "REF"):
            verify(proof, chunk)

    def test_unclaimed_descendant_write_invalidates_ancestor(self):
        proof, chunk = fixture()
        proof["readonly_slots"].pop()
        chunk.protos[2].insns[0] = instruction(0, "SETUPVAL")
        with self.assertRaisesRegex(ValueError, "forwarded write"):
            verify(proof, chunk)

    def test_missing_ancestor_and_circular_proofs_are_rejected(self):
        proof, chunk = fixture()
        missing = copy.deepcopy(proof)
        missing["readonly_slots"].pop(0)
        with self.assertRaisesRegex(ValueError, "ancestor"):
            verify(missing, chunk)
        chunk.protos[1].children = [1]
        proof["readonly_slots"].pop()
        with self.assertRaisesRegex(ValueError, "cycle"):
            verify(proof, chunk)

    def test_refusal_cannot_smuggle_certificates(self):
        proof, _ = fixture()
        proof.update(status="refused", analyzed_slots=None, refusal="budget")
        with self.assertRaisesRegex(ValueError, "refusal carries"):
            verify(proof, None)
        proof["readonly_slots"] = []
        self.assertEqual(verify(proof, None)["analysis_status"], "refused")


if __name__ == "__main__":
    unittest.main()
