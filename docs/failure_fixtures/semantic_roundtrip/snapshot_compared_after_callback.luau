-- `local before = x; return change() < before`: `before` keeps the value `x`
-- had before `change()` wrote it. Moved in as the register `x`, an operand
-- of `<` (or of `>`, the comparison reversed) is read when the comparison
-- runs, after the call, so the snapshot stays (review 2026-10-03, P1).
local function compare()
  local x = 1
  local function change() x = 10 return 5 end
  local before = x
  return change() < before
end
local function branch()
  local x = 1
  local function change() x = 10 return 5 end
  local before = x
  if change() < before then return "yes" end
  return "no"
end
local function subtract()
  local x = 1
  local function change() x = 10 return 5 end
  local before = x
  return before - change(), x
end
print(compare(), branch(), subtract())
