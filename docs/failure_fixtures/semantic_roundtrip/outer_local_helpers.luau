-- Helpers whose only fixed parts are an outer local and a method, inlined at
-- -O2, rebuild as calls; where a short helper matches the start of a longer
-- one's copy, the longer one is rebuilt. Each must keep its effects in order.
local log = {}
local sink = {}
function sink:Emit(value)
	table.insert(log, "emit " .. tostring(value))
end

local function emit(value)
	if value then
		sink:Emit(value)
	end
end

local function run(scope, flag)
	local function purchase(amount)
		scope:Cleanup()
		if scope.OnPurchase(amount) ~= false then
			table.insert(log, "buy " .. tostring(amount))
		end
	end

	local function cancel()
		scope:Cleanup()
	end

	emit(flag and "a")
	emit(nil)
	emit(scope.Name)
	if flag then
		purchase(nil)
	else
		cancel()
	end
	purchase(3)
	return purchase, cancel
end

local scope = { Name = "scope", cleaned = 0 }
function scope:Cleanup()
	self.cleaned += 1
	table.insert(log, "cleanup " .. self.cleaned)
end
function scope.OnPurchase(amount)
	return amount ~= 3
end

run(scope, true)
run(scope, false)
print(table.concat(log, "; "))
