local function nestedExpanded(t) t.branch.value = t.branch.value + 1 end
local function nestedCompound(t) t.branch.value += 1 end
local function keyExpanded(t, key: number) t[key + 1] = t[key + 1] + 1 end
local function keyCompound(t, key: number) t[key + 1] += 1 end
for i, f in {nestedExpanded, nestedCompound} do
 local first, second = {value = 10}, {value = 100}
 local count = 0
 local t = setmetatable({}, {__index = function()
  count += 1
  return if count == 1 then first else second
 end})
 f(t)
 print("base", i, count, first.value, second.value)
end
for i, f in {keyExpanded, keyCompound} do
 local t, count = {10, 20}, 0
 local key = setmetatable({}, {__add = function()
  count += 1
  return count
 end})
 f(t, key)
 print("key", i, count, t[1], t[2])
end
