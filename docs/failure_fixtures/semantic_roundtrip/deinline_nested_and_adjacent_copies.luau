-- Copies of a one-line helper side by side on its line, the constant
-- arguments of the next one loaded between them (`vec(nextNumber(-2, 2),
-- nextNumber(-2, 2))`), and copies inside copies of other helpers at any
-- depth: line info counts each of them, so every copy is a call again.
local random = { seed = 1 }
function random:NextNumber(a, b)
	self.seed = (self.seed * 16807) % 2147483647
	return a + (b - a) * (self.seed / 2147483647)
end
local function nextNumber(a, b)
	return random:NextNumber(a, b)
end

local function vec(x, y, z)
	return { x = x, y = y, z = z }
end

local function jitter(value, width)
	local offset = nextNumber(-width, width)
	if offset > width / 2 then
		print("clamped", offset)
		offset = width / 2
	end
	return value + offset
end

local function wobble(value, width)
	local first = jitter(value, width)
	local second = jitter(first, width / 2)
	print("wobble", first, second)
	return second
end

function scatter(origin)
	local near = vec(nextNumber(-2, 2), nextNumber(-2, 2), nextNumber(-2, 2))
	local far = vec(nextNumber(-4, 4), nextNumber(-4, 4), nextNumber(-4, 4))
	return origin + near.x + far.z, wobble(origin, 3)
end

print(scatter(10))
