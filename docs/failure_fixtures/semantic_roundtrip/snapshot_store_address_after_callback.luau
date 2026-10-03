-- `local savedKey = key; t[savedKey] = change()`: SETTABLE reads a register
-- base or key when it runs, after the value and any store before it, so a
-- snapshot of a cell the value's call writes stays: `t[key] = change()`
-- would store at the new key (review 2026-10-03, P1).
local function key_store(t)
  local key = 1
  local function change() key = 2 return 10 end
  local savedKey = key
  t[savedKey] = change()
  return t[1], t[2]
end
print(key_store({}))
local A, B = {}, {}
local function base_store()
  local target = A
  local function change() target = B return 10 end
  local saved = target
  saved.x = change()
  return A.x, B.x
end
print(base_store())
local function two_stores(t, u)
  local key = 1
  local function change() key = 2 return 10 end
  local savedKey = key
  t[savedKey], u.y = change(), 3
  return t[1], t[2], u.y
end
print(two_stores({}, {}))
