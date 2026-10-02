-- `local callee = Missing.fn` before a branch raises even when the branch is
-- skipped, and an alias of a script table's field keeps the function it read;
-- only a fixed library member (`math.floor`) may be named at its call.
local function run(enabled)
  local callee = Missing.fn
  if enabled then
    callee()
  end
  return "done"
end
print((pcall(run, false)))
ReviewShared = {}
local function snapshot()
  ReviewShared.fn = function() return "old" end
  local callee = ReviewShared.fn
  ReviewShared.fn = function() return "new" end
  return callee()
end
print(snapshot())
local function library(enabled, x)
  local floor = math.floor
  if enabled then
    return floor(x)
  end
  return x
end
print(library(true, 2.5), library(false, 2.5))
