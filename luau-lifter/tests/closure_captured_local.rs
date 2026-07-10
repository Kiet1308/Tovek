//! Regression test for the closure-captured-local `local _ = …` bug.
//!
//! When a local is declared `nil` in a block that dominates the block where it
//! is assigned a closure-capturing value, the SSA open-upvalue analysis used to
//! leave that `nil` declaration out of the upvalue group. The decompiler then
//! emitted a dead `local conn = nil`, re-declared the reassignment as a fresh
//! `local`, collapsed a reader-less captured write to `local _ = …`, and the
//! closures referenced the still-`nil` declaration — a runtime bug
//! (`nil:Disconnect()`). The fix (`cfg/src/ssa/upvalues.rs::extend_open_backward`)
//! pulls the declaration into the cell so every version is one variable.
//!
//! The fixture is the `-O2 -g0` bytecode (decode key 1) of:
//! ```luau
//! local UIS = game:GetService("UserInputService")
//! local conn
//! local conn2
//! if script.Parent then
//!     conn = UIS.InputEnded:Connect(function(i)
//!         if i.UserInputType == Enum.UserInputType.MouseButton1 then
//!             conn:Disconnect(); conn2:Disconnect()
//!         end
//!     end)
//!     conn2 = UIS.InputChanged:Connect(function(i)
//!         if i.UserInputType == Enum.UserInputType.MouseMovement then
//!             conn:Disconnect(); conn2:Disconnect()
//!         end
//!     end)
//! end
//! ```

const BYTECODE: &[u8] = include_bytes!("fixtures/closure_captured_nil_init.luaubc");

#[test]
fn closure_captured_nil_init_is_one_variable() {
    let out = luau_lifter::decompile_bytecode(BYTECODE, 1);

    // The defining symptom: a captured connection whose result has no surviving
    // reader was emitted as a throwaway `local _ = …`.
    assert!(
        !out.contains("local _ ="),
        "regressed: reader-less captured write collapsed to `local _ =`\n{out}"
    );

    // Both connections must be declared once and then plain-assigned (not
    // re-declared with a fresh `local`), and the closures must reference the
    // assigned cell.
    assert!(
        out.contains("connection = "),
        "missing plain assignment for connection:\n{out}"
    );
    assert!(
        out.contains("connection2 = "),
        "missing plain assignment for connection2:\n{out}"
    );
    assert!(
        out.contains("connection:Disconnect()") && out.contains("connection2:Disconnect()"),
        "closures must reference the assigned connections, not dead nil locals:\n{out}"
    );

    // There must be no leftover dead `nil` handle that a closure then disconnects
    // (the pre-fix output declared `local v = nil` / `local v2 = nil` and called
    // `v:Disconnect()` on them).
    assert!(
        !out.contains("v:Disconnect()") && !out.contains("v2:Disconnect()"),
        "a closure still disconnects a dead nil local:\n{out}"
    );
}
