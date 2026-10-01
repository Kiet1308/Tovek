-- A compound update through a register snapshot of a local reads the base
-- once. An upvalue is loaded into a register, so `local t = v; t.k -= 1`
-- prints as `v.k -= 1`; so does a local no closure captures. A local a
-- closure rebinds while the right side runs keeps its snapshot: in place,
-- the store would go to the new table.
local counter = { DependentCount = 2 }
local cleanups = {}
table.insert(cleanups, function()
	counter.DependentCount -= 1
	if counter.DependentCount <= 0 then
		counter = nil
	end
end)
cleanups[1]()
print(counter and counter.DependentCount)
cleanups[1]()
print(counter)

local plain = { Value = 1 }
local alias = plain
alias.Value *= 3
print(plain.Value)

local target = { Count = 1 }
local function swap()
	target = { Count = 100 }
	return 5
end
local first = target
first.Count += swap()
print(first.Count, target.Count)
