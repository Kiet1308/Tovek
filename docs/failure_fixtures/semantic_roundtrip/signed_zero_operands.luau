-- `-0 and 0` is `0`: two zeros of opposite sign are not the same value, as
-- `1 / x` shows.
local function pick(a, b)
  return 1 / (a and b)
end
print(pick(-0, 0))
local a = -0
local b = 0
print(1 / (a and b))
local c = 0
local d = -0
print(1 / (c and d))
