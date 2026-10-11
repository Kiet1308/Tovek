local function stamp(tag)
	local trace = debug.traceback(tag, 1)
	print(tag, select(2, string.gsub(trace, "\n", "")))
end
local function run(x)
	stamp(x)
	return x
end
print(run("a"))
