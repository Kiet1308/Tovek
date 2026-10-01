-- `c and x or d` tests `x` for truthiness after `c`, which structuring
-- printed as `c and x and x or d`. The repeated `x` folds away only when
-- reading it is pure; an indexed read that runs `__index` stays as compiled.
local function pick(c, x, d)
	return c and x or d
end
print(pick(true, 1, 2), pick(true, false, 2), pick(false, 1, 2), pick(true, nil, "d"))

local reads = 0
local proxy = setmetatable({}, {
	__index = function(_, key)
		reads += 1
		return key == "v" and reads or nil
	end,
})
local function viaIndex(c)
	return c and proxy.v or "none"
end
print(viaIndex(true), viaIndex(false), viaIndex(true), reads)
