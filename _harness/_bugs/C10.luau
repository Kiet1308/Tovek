local source = 1
local function test(flag, bump)
	if flag then
		local captured = source
		bump()
		return captured
	end
	return -1
end
local function setter() source = 99 end
local dispatch = {}
dispatch.run = test
local function callIt(t, a, b)
	return t.run(a, b)
end
print(callIt(dispatch, true, setter))
