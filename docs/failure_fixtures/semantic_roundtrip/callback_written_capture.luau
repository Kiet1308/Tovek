-- A temp that reads a captured local may move past a call only when no
-- closure writes that local: then the call cannot change it, and a direct
-- write is a statement of its own. `bump` writes `value`, so `before` must
-- be read before `bump()` runs; `peek` only reads `count`.
local function run(flag)
	local value
	if flag then
		value = 1
	else
		value = 2
	end
	local function bump()
		value = value + 10
		return value
	end
	local before = value
	local after = bump()
	print(after, before)

	local count
	if flag then
		count = 3
	else
		count = 4
	end
	local function peek()
		return count * 2
	end
	local snapshot = count
	local doubled = peek()
	print(doubled, snapshot)
end

run(true)
run(false)
