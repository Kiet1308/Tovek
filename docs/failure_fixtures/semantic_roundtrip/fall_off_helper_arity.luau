-- A helper that returns nothing on some path, inlined where its result is
-- stored: the rebuilt call must still store one value. A call that falls off
-- returns no value, so `return v` must not become `return find(...)`.
local function find(t, k)
	if t[k] then
		return t[k]
	end
end

local function verify(x)
	if x then
		return true
	end
	print("missing")
end

local function outer(t)
	local v = find(t, "q")
	return v
end

local function forwarded(t)
	local v = find(t, "q")
	print(v)
	return v
end

local a = find({ a = 1 }, "a")
local b = find({}, "b")
print(a, b, select("#", find({}, "c")))
print(select("#", outer({})), select("#", outer({ q = 2 })))
print(select("#", forwarded({})))
if not verify(nil) then
	print("no")
end
