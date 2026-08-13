-- VectorD (LBC_CONSTANT_VECTORD, tag 11) literals with special float bits.
-- Every literal is a compile-time vector constant (all opt levels): inf/-inf,
-- signed zero, f64 boundaries past f32 range, subnormals, and a NaN component.
-- NOTE: lifter currently renders vector constants as `Vector3.new(...)`, which is
-- undefined under the plain luau.exe harness runner -> expected RUNDEC_FAIL for
-- the DECOMPILED side until a plain-`vector(...)` printing mode exists.
local vInf = vector(1 / 0, -1 / 0, 2.5)
local vSignedZero = vector(-0.0, 0.0, -0.0)
local vBig = vector(9007199254740993, -9007199254740993, 1.7976931348623157e308)
local vSub = vector(1e-320, 1e-300, 5e-324)
local vOver = vector(1e300, -1e300, 3.4028235e38)
local vNan = vector(0 / 0, 1, 2)
print(vInf)
print(vSignedZero)
print(vBig)
print(vSub)
print(vOver)
print(vNan)
print(vInf + vSignedZero)
print(vBig * 2)
print(vector.zero == vSignedZero)