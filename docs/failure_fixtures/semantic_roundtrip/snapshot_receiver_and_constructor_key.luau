-- NAMECALL reads a register receiver after the arguments, and a constructor's
-- SETTABLE a register key after the field's value: a snapshot of a cell an
-- argument or the value writes keeps the old object or key.
local A = { name = "A" }
function A:m(v) return self.name .. v end
local B = { name = "B", m = A.m }
local function receiver()
  local obj = A
  local function change() obj = B return 1 end
  local saved = obj
  return saved:m(change())
end
local function constructor_key()
  local key = "a"
  local function change() key = "b" return 10 end
  local savedKey = key
  local t = { [savedKey] = change() }
  return t.a, t.b
end
print(receiver(), constructor_key())
