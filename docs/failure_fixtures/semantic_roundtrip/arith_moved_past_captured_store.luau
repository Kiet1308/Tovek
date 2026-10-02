-- An operator that may run a metamethod may not move past a store into a
-- captured cell either.
local function show(...) print(...) end
local cap = 5
local function setcap(x) cap = x end
pcall(function()
  local a = g1 * g2
  setcap(1)
  show(a)
end)
print(cap)
