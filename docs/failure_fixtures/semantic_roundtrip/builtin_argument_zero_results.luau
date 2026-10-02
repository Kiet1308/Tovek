-- A call returning nothing, cut to one value, passes `nil`: `tostring((f()))`
-- is "nil" where `tostring(f())` raises "missing argument". Only a local
-- function's parentheses may go.
local M = {}
function M.find(list, x)
  for i, v in ipairs(list) do
    if v == x then return i end
  end
end
local function check(list, x)
  local r = M.find(list, x)
  return tostring(r)
end
print(pcall(check, {1, 2, 3}, 9))
