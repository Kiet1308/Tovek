-- Helpers returning several values, Luau -O2 inlined at fixed-arity sites.
-- `perp` ends `return unit, unit * axis.k`: its own local shares the first
-- result, the second is declared into a fresh one, so `local right, up =
-- perp(dir)` rebuilds; the same for `setup`. A caller returning `return
-- p, flat(v)` gets `flat`'s copy with `p` cloned into each of its returns:
-- `p` split off, rebuilt as `return p, flat(v)`, also where that is the
-- whole body. That makes `basis`, which holds such a copy, a straight tuple
-- too, and its own copy in `dir` rebuilds as `local look, right =
-- basis(cam)`. `counter` returns a local its closure captures: the copy
-- declares the snapshot the call returns, so `local bump, count =
-- counter(5)` rebuilds and `count` stays 5 while `bump` counts on.
-- Negative: `split` ends with a call taking all its results (`return n,
-- string.find(...)`): no fixed arity, its copy stays.
local function flat(v)
	if v.z == 0 then
		return 0
	end
	return v.x / v.z
end
local function basis(cam)
	local look = flat(cam.look)
	return look, flat(cam.right)
end
local function perp(axis)
	local unit = axis.k > 0.99 and 1 or 2
	return unit, unit * axis.k
end
local function setup(part)
	local size = part.size
	part.size = size * 2
	return size, size + part.size
end
local function counter(start)
	local count = start
	local function bump()
		count += 1
		return count
	end
	return bump, count
end
local function split(s)
	local n = #s
	return n, string.find(s, " ")
end
local M = {}
function M.dir(cam, input)
	local look, right = basis(cam)
	return look * input + right
end
function M.sel(p, v)
	return p, flat(v)
end
function M.cone(dir, k)
	local right, up = perp(dir)
	return dir.k * k + right + up
end
function M.flag(part)
	local size, total = setup(part)
	return size .. "/" .. total .. "/" .. total * 2
end
function M.tick()
	local bump, count = counter(5)
	return bump() + bump(), count
end
function M.words(s)
	local n, at = split(s)
	return n + (at or 0)
end
local cam = { look = { x = 3, z = 4 }, right = { x = 1, z = 0 } }
local a, b = M.sel("x", { x = 6, z = 8 })
print(M.dir(cam, 2), a, b, M.cone({ k = 1 }, 2), M.cone({ k = 0.2 }, 2))
print(M.flag({ size = 3 }), M.words("a b"), M.tick())
