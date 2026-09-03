local trace = {}
local function log(s) trace[#trace + 1] = s end
local obj = setmetatable({}, { __iter = function() log("iter") return next, { a = 1 } end })
local cell = 0
local function run()
	local before = (function() log("before") return {} end)()
	local seen = {}
	local n = 0
	cell = 10
	for k, v in obj do
		seen[k] = v
		n += 1
	end
	log("after")
	return seen, n, before
end
local seen, n = run()
print(table.concat(trace, ","), seen.a, n, cell)
