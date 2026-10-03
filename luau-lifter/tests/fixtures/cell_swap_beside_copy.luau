-- All versions of a captured cell are one variable, and a read names an
-- earlier version even after a later write (`a, b = b, a` in a loop, the
-- cell `a`). At -O2 the inlined `D(a)` copies `a` first; destruction took
-- that copy's value as the cell's for good and joined `b` to the cell, so
-- the swap returned `2, 2`. A cell a closure or a second statement writes
-- names no stable value.
local function D(v)
  local t = type(v)
  return tostring(v)
end
-- loop swap of a cell beside an inlined copy of it
local function swap(x, y)
  local a, b = x, y
  local get = function() return a end
  print(D(a))
  local n = 0
  while n < 2 do
    n = n + 1
    a, b = b, a
  end
  return a, b, get()
end
print(swap(1, 2))
-- the cell written by a closure while a copy taken through a phi is live
local function later(x, flag)
  local a = x
  local set = function() a = a + 10 end
  print(D(a))
  local copy
  if flag then copy = a else copy = 0 end
  set()
  return copy, a
end
print(later(1, true))
print(later(1, false))
-- written in both arms, copied on one
local function arms(x, flag)
  local a = x
  local get = function() return a end
  print(D(a))
  local b = a
  if flag then a = 5 else a = 6 end
  return b, a, get()
end
print(arms(1, true))
