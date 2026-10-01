-- A helper inlined into the value of a statement ran its leading statements
-- before anything that statement evaluates. Here the "prefix" writes the
-- callee the statement reads first, so the call cannot move into place:
-- `dispatch(helper())` reads `dispatch` before `helper` changes it.
function compute()
	return 1
end
local function old(x)
	print("old", x)
end
local function new(x)
	print("new", x)
end
local dispatch = old
local function helper()
	dispatch = new
	return compute()
end
local function run()
	dispatch = new
	dispatch(compute())
end
run()
dispatch = old
print(helper())
