-- `local floor = 0` between a `for` with a `break` and a `while`: folded into
-- the loop's phi, the copy stays in the join both exits of the `for` reach;
-- threaded onto each exit, no structure could hold it (residual goto).
local calls = 0
local function done()
  calls += 1
  return calls % 3 == 0
end
local function F1(stop)
  for it = 1, 2 do
    if stop then break end
  end
  local floor = 0
  while floor < 2 do
    if done() then break end
  end
  return calls
end
print(F1(true))
print(F1(false))
