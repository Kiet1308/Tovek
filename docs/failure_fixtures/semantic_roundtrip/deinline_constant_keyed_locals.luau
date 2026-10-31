-- Copies Luau -O2 made of `refreshDesired(state, key, value)` with constant
-- keys: `LastApplied{key}` is then a constant, and so the local holding
-- it, which Luau folds into its reads (`state.LastAppliedRate`) and drops.
-- The copy also keeps the returned `changed` in the result itself (`v2 =
-- state.DesiredRate ~= rate`). Seeded through those reads and specialized
-- alike, each copy is `refreshDesired(state, "Rate", ...)` again.
local function refreshDesired(state, key, value)
	local appliedKey = `LastApplied{key}`
	local desiredKey = `Desired{key}`
	if state[appliedKey] ~= nil and value == state[appliedKey] then
		return false
	end
	local changed = state[desiredKey] ~= value
	state[desiredKey] = value
	state[appliedKey] = value
	return changed
end

-- `setVisible(false)`: `visible and 0.3 or 0.75` folds to a constant the
-- copy adds where the helper reads its local.
local shown = nil
local items = { { Transparency = 0 }, { Transparency = 0.5 } }
local function setVisible(visible)
	if shown == visible then
		return
	end
	shown = visible
	local offset = visible and 0.3 or 0.75
	for _, item in items do
		item.Transparency = math.clamp(item.Transparency + offset, 0, 1)
	end
end

local function sync(state, instance)
	local changed = refreshDesired(state, "Enabled", instance.Enabled)
	if instance.Kind == "Emitter" then
		changed = refreshDesired(state, "Rate", instance.Rate) or changed
		setVisible(false)
	else
		changed = refreshDesired(state, "Brightness", instance.Brightness) or changed
		setVisible(true)
	end
	return changed
end

local state = {}
print(sync(state, { Enabled = true, Kind = "Emitter", Rate = 5 }))
print(sync(state, { Enabled = true, Kind = "Emitter", Rate = 5 }))
print(sync(state, { Enabled = false, Kind = "Light", Brightness = 2 }))
print(state.DesiredEnabled, state.LastAppliedRate, state.DesiredBrightness)
print(items[1].Transparency, items[2].Transparency)
