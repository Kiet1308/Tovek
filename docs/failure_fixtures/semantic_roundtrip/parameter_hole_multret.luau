-- A parameter holds one value. Where a call's results are all passed on
-- (`print("tag", produce())`), the code is not `helper(produce())`: the
-- helper's parameter keeps only the first. A call already adjusted to one
-- value (`(produce())`) is.
function produce()
	return 11, 22
end

local function helper(p)
	print("tag", p)
end

local function spread()
	print("tag", produce())
end

local function adjusted()
	print("tag", (produce()))
end

spread()
adjusted()
helper(produce())
