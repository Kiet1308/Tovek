local Module = {}

function Module.selected(data)
	local consume = data.consume
	local v2

	if data.condition() then
		v2 = data.make("then")
	else
		v2 = data.make("else")
	end

	return consume({
		[1] = "old",
		v2,
		data.tail()
	})
end

function Module.nested(data)
	local consume = data.consume
	local v2 = data.make("prefix")
	local factory = data.factory()
	local selected

	if data.condition() then
		selected = data.make("then")
	else
		selected = data.make("else")
	end

	return consume({ v2, factory({
			Name = "Panel",
			Selected = selected,
			[data.key()] = data.make("property"),
			Handler = function()
				return data.make("callback")
			end
		}), data.tail() })
end

function Module.snapshot(data)
	local snapshot = "initial"

	local function change()
		snapshot = "changed"
		return data.condition()
	end

	local consume = data.consume
	local v2 = {
		Snapshot = snapshot
	}
	snapshot = "changed"
	local v3

	if data.condition() then
		v3 = data.make("then")
	else
		v3 = data.make("else")
	end

	do local _values = table.pack(v3, data.tail()); for _k = 1, _values.n do v2[_k] = _values[_k] end end
	return consume(v2)
end

function Module.observed(data)
	local v = { "old" }
	local v2

	if data.inspect(v) then
		v2 = data.make("then")
	else
		v2 = data.make("else")
	end

	v[1] = v2
	v[2] = data.make("property")
	return data.consume(v)
end

function Module.captured(data)
	local v = { "old" }
	data.register(function()
		return v
	end)
	local v2

	if data.condition() then
		v2 = data.make("then")
	else
		v2 = data.make("else")
	end

	v[1] = v2
	v[2] = data.make("property")
	return data.consume(v)
end

function Module.initializer(data)
	local consume = data.consume
	local v = {
		Snapshot = data.make("initial"),
		[data.key()] = data.make("key-value")
	}
	local v2

	if data.condition() then
		v2 = data.make("then")
	else
		v2 = data.make("else")
	end

	do local _values = table.pack(v2, data.tail()); for _k = 1, _values.n do v[_k] = _values[_k] end end
	return consume(v)
end

function Module.interleaved(data)
	if data.condition() then
		local receiver = data.receiver()
		receiver[data.key()] = data.make("then")
	else
		local receiver_2 = data.receiver()
		receiver_2[data.key()] = data.make("else")
	end

	local v = {
		Header = "fixed",
		Selected = data.make("property")
	}
	return data.consume(v)
end

return Module
