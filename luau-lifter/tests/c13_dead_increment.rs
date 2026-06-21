//! Regression test for the C13 dead self-update lost-write
//! (`cfg/src/ssa/destruct.rs::coalesce_dead_register_writes`).
//!
//! Out-of-SSA destruction coalesces only copies, phis and upvalue groups, so a
//! NON-copy self-update (`count = count + 1`) whose result no surviving
//! statement reads — a counter incremented on a straight-line / dead-on-path
//! branch — used to become a singleton congruence class → a fresh local →
//! named `_`, emitting a throwaway `local _ = count + 1` that SILENTLY DROPS the
//! increment. Inside a loop the same write was rescued by the loop-header phi,
//! so the bug only ever showed on the no-phi shape (this is exactly the
//! `MeteorShower.luau` corpus case: `local _ = v4 + 1` on two dead branches while
//! the loop bodies correctly showed `v4 += 1`).
//!
//! The fix merges a dead self-update back into the variable's congruence class
//! when its RHS reads the SAME original register as its destination
//! (interference-gated), so it renders as `count += 1`.
//!
//! Fixture: `-O2 -g0` v11 bytecode (decode key 1) of:
//! ```luau
//! local function mount(attachment, instance)
//!     local count = 0
//!     if attachment:IsA("Attachment") then
//!         attachment:Clone().Parent = instance
//!         count += 1                      -- dead-on-path → was `local _ = count + 1`
//!     else
//!         for _, child in attachment:GetChildren() do
//!             child:Clone().Parent = instance
//!             count += 1                  -- loop phi → always survived
//!         end
//!         if count == 0 then
//!             instance.Name = "empty"
//!             count += 1                  -- dead-on-path → was `local _ = count + 1`
//!         end
//!     end
//!     print(instance)
//! end
//! ```

const BYTECODE: &[u8] = include_bytes!("fixtures/c13_counter_self_update.luaubc");

#[test]
fn dead_self_update_is_not_dropped_to_local_underscore() {
    let out = luau_lifter::decompile_bytecode(BYTECODE, 1);

    // The defining symptom: a dead increment collapsed to a throwaway local that
    // computes `count + 1` and discards it (losing the write).
    assert!(
        !out.contains("local _ ="),
        "regressed: a dead self-update collapsed to `local _ =`\n{out}"
    );

    // All three increments — the two former dead-on-path sites and the loop body —
    // must survive as real writes. The formatter renders a self-update as `+= 1`,
    // so exactly three compound increments must appear (assert STRUCTURE, not the
    // generated `vN` name, which `name_locals` may choose differently).
    let increments = out.matches("+= 1").count();
    assert_eq!(
        increments, 3,
        "expected 3 surviving `+= 1` self-updates (2 former dead branches + 1 loop), got {increments}\n{out}"
    );
}
