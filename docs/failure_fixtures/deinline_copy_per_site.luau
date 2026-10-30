-- Reduced from Part_Icles (real dump): `originFor` and `ctxFor` both copy the
-- parameter `cframe`; a later `local final = cframe; if ... then final = ... end`
-- coalesces into it, so `cframe` is written after both copies. No write runs
-- between the two closures' creation, so the copy inside `ctxFor` agrees with
-- `originFor`'s and its inlined call is rebuilt; a site reading the parameter
-- directly in `emit` keeps the module-wide proof.
local function emit(folder, cframe, options)
	local rot = nil
	if folder.model then
		rot = folder.pivot
	end
	local function originFor(p)
		if not rot then
			return cframe
		end
		return cframe * p
	end
	local function ctxFor(instance)
		local ctx = { A = 1 }
		if options.skip then
			return ctx
		end
		ctx.Origin = originFor(instance)
		return ctx
	end
	for _, child in folder.children do
		print(ctxFor(child).Origin)
	end
	local final = cframe
	if options.full == false and rot then
		final = cframe + rot
	end
	return final, ctxFor
end
local f, ctx = emit({ model = true, pivot = 3, children = { 1, 2 } }, 10, { full = false })
print(f, ctx(5).Origin)
