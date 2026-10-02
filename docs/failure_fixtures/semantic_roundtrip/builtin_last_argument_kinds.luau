-- Parenthesized last arguments of builtins and a vararg counter given zero,
-- one or several results: `type((f()))`, `rawset(t, k, (f()))` and
-- `setmetatable(t, (f()))` pass `nil` where `f()` alone passes nothing.
local function check(produce)
  return type((produce()))
end
print(check(function() end))
print(check(function() return 1, 2 end))
local function clear(t, produce)
  rawset(t, "key", (produce()))
  return t.key
end
print(clear({ key = 1 }, function() end))
local function strip(t, produce)
  setmetatable(t, (produce()))
  return getmetatable(t)
end
print(strip(setmetatable({}, {}), function() end))
local function show(...)
  return select("#", ...)
end
local function count(produce)
  return show((produce()))
end
print(count(function() end), count(function() return 1, 2 end))
