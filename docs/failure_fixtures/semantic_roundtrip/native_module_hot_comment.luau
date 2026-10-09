--!native
-- The `--!native` hot comment sets the main prototype's LPF_NATIVE_MODULE
-- flag (and native compile options). The lifter parsed prototype flags and
-- dropped them, so the output lost the directive; it now leads the output
-- whenever the flag is set. NATIVE_COLD alone (top-level code without loops)
-- prints nothing.
local function sum(n)
	local total = 0
	for i = 1, n do
		total += i * i
	end
	return total
end

print(sum(10), sum(100))
