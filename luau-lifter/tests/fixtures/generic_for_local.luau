local Util = {}

function Util.DeepCopy(tbl)
	local newTbl = table.clone(tbl)
	for k, v in newTbl do
		if type(v) == "table" then
			newTbl[k] = Util.DeepCopy(v)
		end
	end
	return newTbl
end

function Util.Keys(make)
	local keys = {}
	for k in (make()), nil, nil do
		table.insert(keys, k)
	end
	return keys
end

function Util.Count(make)
	local items = make()
	local count = 0
	for _ in items do
		count += 1
	end
	return count
end

return Util
