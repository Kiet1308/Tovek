local function reflect(value, normal)
    local dot = value.X * normal.X + value.Y * normal.Y + value.Z * normal.Z
    return value - normal * (2 * dot)
end
local function scaled(normal)
    return normal * 2
end
local function reflected(value)
    return reflect(value, vector.create(0, 1, 0)), reflect(value, vector.create(1, 0, 0))
end
for _, value in {vector.create(1, 2, 3), vector.create(-2, -4, 0)} do
    print("reflect", reflected(value))
end
print("scaled", scaled(vector.create(1, 2, 3)), scaled(vector.create(-1, -0, 0.5)))
print("rounded", scaled(vector.create(16777216, -0, 0.1)))
local saved = vector.create
local replacements = 0
local fake = {create = function()
    replacements += 1
    return saved(4, 5, 6)
end}
local function dynamic(vector)
    return scaled(vector.create(1, 2, 3))
end
print("mutable", dynamic(fake), replacements)
