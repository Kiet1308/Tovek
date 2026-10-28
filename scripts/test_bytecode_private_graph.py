import unittest
from unittest.mock import patch

import bytecode_graph as private
import test_bytecode_graph as existing
from bytecode_graph import compare_private_graph as compare
from bytecode_dataflow import compare_dataflow
from test_bytecode_dataflow import chunk, instruction as ins


class PrivateCopies(unittest.TestCase):
    def pair(self):
        a = chunk([ins('GETGLOBAL', 2, aux=0), ins('MOVE', 3, 0), ins('CALL', 2, 2, 2), ins('RETURN', 2, 2)], constants=[('str',1)], strings=[b'f'])
        b = chunk([ins('GETGLOBAL', 2, aux=0), ins('MOVE', 3, 2), ins('MOVE', 4, 0), ins('CALL', 3, 2, 2), ins('RETURN', 3, 2)], constants=[('str',1)], strings=[b'f'])
        return a,b
    def test_positive_adjacent_private_lookup_and_budget(self):
        a,b=self.pair();self.assertEqual(compare(a,b)['status'],'proved')
        self.assertEqual(compare(a,b,budget=2)['status'],'unknown')
    def test_does_not_reorder_observable_lookup(self):
        a,b=self.pair();b.strings=[b'g'];self.assertEqual(compare(a,b)['status'],'unknown')
    def test_source_liveness_through_both_branches(self):
        a=chunk([ins('GETGLOBAL',2,aux=0),ins('MOVE',3,2),ins('JUMPIF',0,d=1),ins('RETURN',2,2),ins('RETURN',3,2)],constants=[('str',1)],strings=[b'f'])
        graph,stats=private.Graph(a,20000,private_copies=True).build()
        self.assertEqual(stats['private_copies_removed'],0)
    def test_source_liveness_through_backedge(self):
        a=chunk([ins('GETGLOBAL',2,aux=0),ins('MOVE',3,2),ins('JUMPIF',0,d=2),ins('MOVE',1,2),ins('JUMPBACK',d=-3),ins('RETURN',3,2)],constants=[('str',1)],strings=[b'f'])
        graph,stats=private.Graph(a,20000,private_copies=True).build()
        self.assertEqual(stats['private_copies_removed'],0)
    def test_multiple_predecessor_copy_cannot_be_removed(self):
        a=chunk([ins('JUMPIF',0,d=2),ins('GETGLOBAL',2,aux=0),ins('MOVE',3,2),ins('RETURN',3,2)],constants=[('str',1)],strings=[b'f'],params=3)
        graph,stats=private.Graph(a,20000,private_copies=True).build()
        self.assertEqual(stats['private_copies_removed'],0)
    def test_preserves_fixed_result_and_argument_arity(self):
        a,b=self.pair();b.protos[0].insns[3]=(4,*ins('CALL',3,1,2));self.assertEqual(compare(a,b)['status'],'unknown')
    def test_open_pack_function_remains_exact(self):
        a,b=self.pair()
        a.protos[0].insns[-2]=(3,*ins('CALL',2,2,0));a.protos[0].insns[-1]=(4,*ins('RETURN',2,0))
        b.protos[0].insns[-2]=(4,*ins('CALL',3,2,0));b.protos[0].insns[-1]=(5,*ins('RETURN',3,0))
        self.assertEqual(compare(a,b)['status'],'unknown')
    def test_in_place_lookup_reads_old_value_and_writes_new_lifetime(self):
        a=chunk([ins('GETGLOBAL',2,aux=0),ins('GETTABLEKS',2,2,aux=1),ins('MOVE',3,0),ins('CALL',2,2,2),ins('RETURN',2,2)],constants=[('str',1),('str',2)],strings=[b'library',b'f'])
        b=chunk([ins('GETGLOBAL',2,aux=0),ins('GETTABLEKS',2,2,aux=1),ins('MOVE',3,2),ins('MOVE',4,0),ins('CALL',3,2,2),ins('RETURN',3,2)],constants=[('str',1),('str',2)],strings=[b'library',b'f'])
        self.assertEqual(compare(a,b)['status'],'proved')
        b.protos[0].insns[1]=(2,*ins('GETTABLEKS',2,0,aux=1))
        self.assertEqual(compare(a,b)['status'],'unknown')
    def test_parameter_old_value_and_overwritten_lifetime(self):
        a,b=self.pair()
        a.protos[0].insns[-1]=(4,*ins('MOVE',0,2));a.protos[0].insns.append((5,*ins('RETURN',0,2)))
        b.protos[0].insns[-1]=(5,*ins('MOVE',0,3));b.protos[0].insns.append((6,*ins('RETURN',0,2)))
        self.assertEqual(compare(a,b)['status'],'proved')
        b.protos[0].insns[-2]=(5,*ins('MOVE',0,1))
        self.assertEqual(compare(a,b)['status'],'unknown')
    def test_value_capture_keeps_source_live(self):
        child=chunk([ins('GETUPVAL',0,0),ins('RETURN',0,2)],params=0,upvalues=1).protos[0];child.id=1
        a=chunk([ins('GETGLOBAL',2,aux=0),ins('MOVE',3,2),ins('NEWCLOSURE',4,d=0),ins('CAPTURE',0,2),ins('RETURN',3,3)],constants=[('str',1)],strings=[b'f'])
        a.protos[0].children=[1];a.protos.append(child)
        graph,stats=private.Graph(a,20000,private_copies=True).build()
        self.assertEqual(stats['private_copies_removed'],0)
    def test_lookup_effect_between_producer_and_copy_is_not_removed(self):
        a=chunk([ins('GETGLOBAL',2,aux=0),ins('GETGLOBAL',4,aux=1),ins('MOVE',3,2),ins('RETURN',3,2)],constants=[('str',1),('str',2)],strings=[b'f',b'g'])
        graph,stats=private.Graph(a,20000,private_copies=True).build()
        self.assertEqual(stats['private_copies_removed'],0)
    def test_reference_chunk_keeps_exact_storage(self):
        a,b=self.pair()
        a.protos[0].num_upvalues=b.protos[0].num_upvalues=1
        self.assertEqual(compare(a,b)['status'],'unknown')

    def test_existing_graph_mutants_under_private_certificate(self):
        for name in unittest.defaultTestLoader.getTestCaseNames(existing.GraphTests):
            with self.subTest(control=name), patch.object(existing, 'compare_graph', compare), patch.object(existing, 'compare_dataflow', compare):
                result = unittest.TestResult()
                existing.GraphTests(name).run(result)
                self.assertTrue(result.wasSuccessful(), result.failures + result.errors)

    def test_vararg_preparation_is_unique_entry_and_never_reentered(self):
        cases = [
            chunk([ins('RETURN', 0, 2)], params=1, vararg=True),
            chunk([ins('NOP'), ins('PREPVARARGS', 1), ins('RETURN', 0, 2)], params=1, vararg=True),
            chunk([ins('PREPVARARGS', 1), ins('PREPVARARGS', 1), ins('RETURN', 0, 2)], params=1, vararg=True),
            chunk([ins('PREPVARARGS', 1), ins('JUMPIF', 0, d=-2), ins('RETURN', 0, 2)], params=1, vararg=True),
        ]
        for malformed in cases:
            with self.subTest(code=malformed.protos[0].insns):
                for checker in (compare, private.compare_graph, compare_dataflow):
                    self.assertEqual(checker(malformed, malformed)['status'], 'unknown')
        valid = chunk([ins('PREPVARARGS', 1), ins('RETURN', 0, 2)], params=1, vararg=True)
        for checker in (compare, private.compare_graph, compare_dataflow):
            self.assertEqual(checker(valid, valid)['status'], 'proved')
