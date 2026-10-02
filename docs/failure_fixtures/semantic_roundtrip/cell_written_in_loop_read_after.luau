-- A closure made in a loop writes a captured local the loop never assigns;
-- the read after the loop sees every write, not a snapshot from before it.
local function c4(n)
  local state = 0
  for i = 1, n do
    local add = function(x) state = state + x end
    add(i)
  end
  return state
end
print(c4(5))
local function c4b(list)
  local total = 0
  local count = 0
  for _, v in ipairs(list) do
    local function bump() total += v; count += 1 end
    if v > 1 then bump() end
  end
  return total, count
end
print(c4b({1, 2, 3}))
local function c4c(n)
  local s = ""
  local i = 0
  while i < n do
    i += 1
    task = function() s = s .. i end
    task()
  end
  return s
end
print(c4c(3))
