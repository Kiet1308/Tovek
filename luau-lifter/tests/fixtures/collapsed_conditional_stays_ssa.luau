-- Collapsing `if a then a = v end` into `a = a and v` assigned the join's
-- phi parameter itself while other paths still reached that phi: a second
-- definition outside SSA form. Destruction missed that the old value was
-- live across it and joined two variables: the swap read the new `a`, and
-- `bestCount = nil` overwrote the stack count before `math.min(x, x)`
-- (Sky Land's PlantPredictionClient, DivergentVFX static_mesh).
local function m2(a, b) return b, a end
-- `if a then a = ... end` collapses into `a = a and ...`; the swap after it
-- must still read the old `a`
local function swap(x, y)
  local cl = {}
  local a, b, c = m2(x, y)
  a, b = m2(false, 0)
  if a then
    if b then
      cl[1] = function(z) c = z return c end
    elseif a then
      a = (tonumber(b) or 1) + (b or 0)
    end
  end
  if not b then
    c, a = a, c
  end
  return a, b, c
end
print(swap(1, 2))
-- a best count from a stack, narrowed by an inventory count
local function best(stack, entry)
  local bestCount = nil
  if stack then
    local count = tonumber(stack)
    if count then
      bestCount = count
    end
  end
  local inventoryCount = nil
  if type(entry) == "table" then
    inventoryCount = tonumber(entry.Count)
  elseif type(entry) == "number" then
    inventoryCount = entry
  end
  if inventoryCount then
    if bestCount then
      bestCount = math.min(bestCount, inventoryCount)
    else
      bestCount = inventoryCount
    end
  end
  return bestCount
end
print(best("5", 3), best("5", { Count = 9 }), best(nil, 4), best("2", nil), best(nil, nil))
