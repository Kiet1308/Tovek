-- Keep both parameters live beyond the select so Luau allocates a distinct
-- result register. The helper's bias has unknown truthiness; the copied site
-- specializes it to the truthy number 2.
local function nestedCurve(value, bias)
    return -(((if value < 0 then bias else value * 3) + bias) + value)
end

for _, value in { -2, -0.0, 0.0, 2, 0 / 0 } do
    print("nested-helper", nestedCurve(value, 2))
    print("nested-copy", -(((value < 0 and 2 or value * 3) + 2) + value))
end

return nestedCurve
