-- A generic `for` whose body only breaks: from -O1 the compiler threads the
-- loop's backedge through that `break`, so FORGLOOP jumps past its AUX
-- whether the generator returns a value or not. The loop still makes its
-- first call. The decompile used to fail structuring ("residual goto").
-- Found by the typed fuzzer (`if -1 <= 256 then break end` folds to it).
local calls = 0
local function iterate(limit)
	local index = 0
	return function()
		calls += 1
		index += 1
		if index <= limit then
			return index
		end
		return nil
	end
end

local function first(items)
	for _ in items do
		break
	end
	return 1
end

local function folded()
	for _ in iterate(3) do
		if -1 <= 256 then
			break
		end
	end
	return calls
end

print(first({ 1, 2 }), folded())
