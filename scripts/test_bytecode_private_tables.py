import copy
import unittest
import bytecode_dataflow as model
from test_bytecode_dataflow import chunk, instruction as op


def table_chunk(code, template=None):
    return chunk(code, params=0, constants=[('str',1),('str',2),template or ('table',(0,1)),('str',3)], strings=[b'a',b'b',b'callback'])

class PrivateTablesTests(unittest.TestCase):
    def assert_status(self,a,b,status):
        self.assertEqual(model.compare_dataflow(a,b)['status'],status)

    def test_fresh_allocation_and_exact_zero_stores_move_no_calls(self):
        original=table_chunk([op('DUPTABLE',0,d=2),op('GETGLOBAL',2,aux=3),op('CALL',2,1,2),op('SETTABLEKS',2,0,aux=0),op('RETURN',0,2)])
        restored=table_chunk([op('GETGLOBAL',2,aux=3),op('DUPTABLE',0,d=2),op('LOADN',3,d=0),op('SETTABLEKS',3,0,aux=1),op('CALL',2,1,2),op('SETTABLEKS',2,0,aux=0),op('RETURN',0,2)])
        self.assert_status(original,restored,'proved')
        bad=copy.deepcopy(restored); bad.protos[0].insns[-2]=(bad.protos[0].insns[-2][0],*op('SETTABLEKS',2,0,aux=1))
        self.assert_status(original,bad,'different')

    def test_nil_deletion_before_later_overwrite_remains_different(self):
        left=[op('DUPTABLE',0,d=2),op('GETGLOBAL',2,aux=3),op('CALL',2,1,2),op('SETTABLEKS',2,0,aux=0),op('LOADN',3,d=9),op('SETTABLEKS',3,0,aux=1),op('RETURN',0,2)]
        right=[left[0],op('LOADNIL',3),op('SETTABLEKS',3,0,aux=1),*left[1:]]
        self.assert_status(table_chunk(left),table_chunk(right),'different')

    def test_escape_prevents_redundant_slot_store_proof(self):
        for escape in ([op('GETGLOBAL',2,aux=3),op('MOVE',3,0),op('CALL',2,2,1)],
                       [op('SETGLOBAL',0,aux=3)], [op('SETUPVAL',0,0)],
                       [op('GETTABLEKS',2,0,aux=0)]):
            with self.subTest(escape=escape):
                left=[op('DUPTABLE',0,d=2),*escape,op('RETURN',0,2)]
                right=[*left[:-1],op('LOADN',3,d=0),op('SETTABLEKS',3,0,aux=0),left[-1]]
                a,b=table_chunk(left),table_chunk(right)
                if escape[0][0]==op('SETUPVAL')[0]:
                    def closed(value):
                        child = value.protos[0]
                        child.id, child.num_upvalues = 1, 1
                        outer = chunk([op('NEWCLOSURE',1,d=0),op('CAPTURE',0,0),op('RETURN',1,2)],
                                      params=1, strings=value.strings)
                        outer.protos[0].children = [1]
                        outer.protos.append(child)
                        return outer
                    a, b = closed(a), closed(b)
                self.assert_status(a,b,'different')

    def test_template_order_duplicate_capacity_and_signed_zero_remain_visible(self):
        code=[op('DUPTABLE',0,d=2),op('RETURN',0,2)]
        original=table_chunk(code)
        for template in [('table',(1,0)),('table',(0,1,0)),('table',(0,))]:
            self.assert_status(original,table_chunk(code,template),'different')
        zero=table_chunk(code,('tablek',((0,-1),(1,-1))))
        self.assert_status(original,zero,'proved')
        for scalar in [('num',-0.0),('nil',),('bool',False)]:
            mutant=copy.deepcopy(zero); mutant.protos[0].constants.append(scalar)
            mutant.protos[0].constants[2]=('tablek',((0,4),(1,-1)))
            self.assert_status(original,mutant,'different')

    def test_nil_template_allocation_is_not_hoisted_across_effects(self):
        left=[op('DUPTABLE',0,d=2),op('GETGLOBAL',2,aux=3),op('RETURN',0,2)]
        right=[left[1],left[0],left[2]]
        a,b=table_chunk(left,('tablek',((0,4),(1,-1)))),table_chunk(right,('tablek',((0,4),(1,-1))))
        a.protos[0].constants.append(('nil',));b.protos[0].constants.append(('nil',))
        self.assert_status(a,b,'different')

    def test_dynamic_key_write_and_self_reference_end_private_slot_proof(self):
        for middle in ([op('GETGLOBAL',3,aux=3),op('LOADN',4,d=8),op('SETTABLE',4,0,3)],
                       [op('SETTABLEKS',0,0,aux=1)]):
            left=[op('DUPTABLE',0,d=2),*middle,op('RETURN',0,2)]
            right=[*left[:-1],op('LOADN',3,d=0),op('SETTABLEKS',3,0,aux=0),left[-1]]
            self.assert_status(table_chunk(left),table_chunk(right),'different')

    def test_child_event_ids_are_not_rebased_with_parent_events(self):
        child=chunk([op('GETGLOBAL',0,aux=0),op('RETURN',0,2)],params=0,constants=[('str',1)],strings=[b'callback']).protos[0]
        child.id=1
        def make(prefix):
            c=table_chunk([*prefix,op('NEWCLOSURE',1,d=0),op('RETURN',0,3)])
            c.protos[0].children=[1];c.protos.append(copy.deepcopy(child));return c
        a=make([op('GETGLOBAL',2,aux=3),op('DUPTABLE',0,d=2)])
        b=make([op('DUPTABLE',0,d=2),op('GETGLOBAL',2,aux=3)])
        self.assert_status(a,b,'proved')
        b.protos[1].insns[-1]=(2,*op('RETURN',1,2))
        self.assertNotEqual(model.compare_dataflow(a,b)['status'],'proved')

    def test_branch_arm_offsets_and_truthy_collapse_keep_parent_values(self):
        prefix=[op('GETGLOBAL',1,aux=3),op('JUMPIFNOT',0,d=4)]
        then=[op('DUPTABLE',2,d=2),op('GETGLOBAL',3,aux=3),op('RETURN',2,2)]
        moved=[op('GETGLOBAL',3,aux=3),op('DUPTABLE',2,d=2),op('RETURN',2,2)]
        otherwise=[op('LOADNIL',4),op('RETURN',4,2)]
        def make(code):
            c=table_chunk(code);c.protos[0].num_params=1;return c
        self.assert_status(make([*prefix,*then,*otherwise]),make([*prefix,*moved,*otherwise]),'proved')
        wrong=[*moved[:-1],op('RETURN',1,2)]
        self.assert_status(make([*prefix,*then,*otherwise]),make([*prefix,*wrong,*otherwise]),'different')
        self.assert_status(make([*prefix,*then,*then]),make([*prefix,*then,*moved]),'proved')

    def test_private_table_budget_fails_closed(self):
        c=table_chunk([op('DUPTABLE',0,d=2),op('RETURN',0,2)])
        self.assertEqual(model.compare_dataflow(c,c,budget=2)['status'],'unknown')
        from bytecode_private_tables import normalize_private_tables
        with self.assertRaises(model.Unknown):
            normalize_private_tables((), ('branch', ('truthy', ('parameter', 0))), 0, [100])

if __name__=='__main__':
    unittest.main()
