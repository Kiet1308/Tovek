-- `t.x = v`: Luau reads a register local `t` (and key) when the store runs,
-- after the value, so `swap()` writing `t` may fold into the store; an
-- upvalue target is fetched first and keeps its snapshot.
local function h()
  local t = {}
  local other = {}
  local function swap() t = other return 1 end
  local v = swap()
  t.x = v
  return t == other, other.x, rawget(t, "x")
end
print(h())
local function k()
  local t = {}
  local first = t
  local other = {}
  local function swap() t = other return "k" end
  local key = swap()
  first[key] = 1
  t[key] = 2
  return first.k, other.k
end
print(k())
local function m()
  local t = {}
  local other = {}
  local function swap() t = other return 1 end
  return function()
    local target = t
    local v = swap()
    target.y = v
    return t == other, other.y, target.y
  end
end
print(m()())
