//! A module's methods read as `function M:m()` even while it is a constructor.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false module_methods.luau`.
//! At -O2 the module is still `return { RemoteEvent = function(_, name) ...,
//! Connect = function(p, ...) p:RemoteEvent(name) ... }` when methods are
//! recovered; only `Prefix.m = function` definitions were considered, so
//! RbxUtil `Net:Connect` printed as `Module.Connect(object, ...)`.

const BYTECODE: &[u8] = include_bytes!("fixtures/module_methods.luaubc");

#[test]
fn constructor_methods_take_self() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(source.contains("function Module:RemoteEvent("), "{source}");
    assert!(source.contains("function Module:Connect("), "{source}");
    assert!(source.contains("return self:RemoteEvent("), "{source}");
}
