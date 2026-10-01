-- `local r = f(); return r` returns one value; `return f()` returns every
-- value of f's call. Only a helper proven to return exactly one value may
-- collapse that way, and the proof must come from f's own body, which a
-- rebuilt declaration (`local f = factory()`) no longer shows.
function produce(tag)
	return tag, 22
end

local function factory()
	return function()
		return produce("tag")
	end
end

local f = factory()

local function caller()
	local r = produce("tag")
	return r
end

print(select("#", caller()), caller())
print(select("#", f()))
