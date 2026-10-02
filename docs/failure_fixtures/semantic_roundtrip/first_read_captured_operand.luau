-- A helper reading a cell the moved argument's call writes (`x`) before its
-- parameter: the inlined copy read `x` first, so no call is rebuilt.
local x = 1
local obj = {}
function obj.bump() x = x + 10; return 5 end
local function h(t)
  return string.format("%d %d %s", x, t, "z") .. "!"
end
print(h(2))
print(string.format("%d %d %s", x, obj.bump(), "z") .. "!")
print(x)
