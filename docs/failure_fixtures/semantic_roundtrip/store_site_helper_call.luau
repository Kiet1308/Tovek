-- A helper's inlined value stored into a captured register local's field
-- (`clone.Position = calcPos(pos)`): the store reads `clone` after the call
-- either way, so the call is rebuilt in place.
local Vec = {}
Vec.__index = Vec
function Vec.new(x, y) return setmetatable({x = x, y = y}, Vec) end
function Vec:Lerp(o, a) return Vec.new(self.x + (o.x - self.x) * a, self.y + (o.y - self.y) * a) end
local function run(data, part)
  local total = 1
  local startPos = data.StartPos
  local clone = part
  local function calcPos(p)
    local v = math.min(1, total / 2)
    local v2 = v * v
    return Vec.new(startPos.x, p.y):Lerp(p, v2)
  end
  clone.Position = calcPos(data.Pos)
  local function abort() clone = nil end
  return function(dt)
    total += dt
    if total > 3 then abort() return "aborted" end
    clone.Position = calcPos(data.Pos)
    return clone.Position.x, clone.Position.y
  end
end
local part = {}
local step = run({StartPos = Vec.new(0, 0), Pos = Vec.new(4, 8)}, part)
print(part.Position.x, part.Position.y)
print(step(0.5))
print(step(5))
