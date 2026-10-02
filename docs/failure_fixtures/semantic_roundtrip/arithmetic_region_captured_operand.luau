-- `local q = c * 2 + 1; return y * q + 1` with `__mul` writing `y`: Luau reads
-- the register `y` when `*` runs, after `c * 2`; a rebuilt `scale(y, c)`
-- would read it first.
local function scale(a, b)
  local q = b * 2 + 1
  return a * q + 1
end
print(scale(2, 3))
local y = 3
local c = setmetatable({}, {__mul = function(l, r) y = 100; return 7 end})
local function site()
  local q = c * 2 + 1
  return y * q + 1
end
print(site(), y)
