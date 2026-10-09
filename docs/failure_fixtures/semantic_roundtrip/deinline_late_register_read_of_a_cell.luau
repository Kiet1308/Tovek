-- A local of the running function is read when the operation using it runs,
-- after the operation's other operands: in `cache + hooks.bump()` Luau reads
-- `cache` after `hooks.bump()` changed it. A helper ending `return cache`
-- reads it before. So the statement after a copy of `refresh` (written out
-- by hand here) must not take `refresh(n)` in place of that read: the copy
-- stays a call statement and the sum keeps reading `cache`. Before the fix
-- the de-inliner printed `local x = refresh(n) + hooks.bump()`, which adds
-- the old value (41 for 1001). The same holds for a helper whose every path
-- returns `cache` (`load`), whose call for no result would otherwise hand
-- its value to the read. Where the local is an upvalue, fetched where it
-- stands, the call does take it (`refresh(n) .. hooks.tag()` in `remote`).
local cache = 0
local function refresh(n)
	cache = math.floor(n) * 10
	return cache
end
local function load(n)
	if n then
		cache = math.floor(n) * 100
		assert(cache, "")
	end
	return cache
end
local hooks = {}
function hooks.bump()
	cache = 1000
	return 1
end
function hooks.tag()
	cache = 2000
	return "!"
end

print(refresh(3), load(1))
local n = tonumber("4")
print(n)
cache = math.floor(n) * 10
local x = cache + hooks.bump()
print(x, cache)
if n then
	cache = math.floor(n) * 100
	assert(cache, "")
end
local y = cache + hooks.bump()
print(y, cache)
local function remote(m)
	cache = math.floor(m) * 10
	return cache .. hooks.tag()
end
print(remote(5), cache)
