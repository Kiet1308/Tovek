-- `("%*%d"):format(x, n)` cannot become a backtick string (`%d`). Deciding
-- that must not render `x` first: nested twenty deep, each level would
-- render its argument again and double the work.
local function run(leaf)
	return ("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(("%*%d"):format(leaf, 0), 1), 2), 3), 4), 5), 6), 7), 8), 9), 10), 11), 12), 13), 14), 15), 16), 17), 18), 19)
end
print(run("x"))
