return function(condition, make, observe, initial, key)
	local props = { Value = initial, Name = "Panel", [1] = "seed" }
	if condition() then
		props.Value = make("then")
	else
		props.Value = make("else")
	end
	props[key] = make("child")
	return observe(props)
end
