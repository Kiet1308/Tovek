-- `math.abs.missing` indexes a function and raises where it is read: an
-- alias of it stays before a branch that skips the call, and before an
-- effect that would otherwise run first.
local out = {}
local function skipped(flag)
  local callee = math.abs.missing
  if flag then
    callee()
  end
  return "done"
end
local function after_effect()
  local callee = math.abs.missing
  table.insert(out, "between")
  callee()
end
table.insert(out, tostring((pcall(skipped, false))))
table.insert(out, tostring((pcall(after_effect))))
print(table.concat(out, ","))
