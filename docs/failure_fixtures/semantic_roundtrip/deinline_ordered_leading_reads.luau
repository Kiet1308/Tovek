-- Several arguments that run code, moved back into one rebuilt call. Luau's
-- -O2 copy evaluates every argument in parameter order before the body;
-- SSA then folds each one into its parameter's single read
-- (`Service:Create(owner, table.remove(q, 1), { ... })`). The call
-- `animate(table.remove(q, 1), { FieldOfView = table.remove(q, 1) })` keeps
-- that order exactly when the body reads those parameters first, in
-- parameter order, with nothing observable before or between them: the
-- ordered leading reads. Each `table.remove` takes the next queued value, so
-- a reordering would show in the result.
local created = {}
local Service = {}
function Service:Create(target, info, props)
	table.insert(created, info)
	return { target = target, info = info, props = props }
end

local function queued(owner, queue)
	local function animate(info, props)
		local t = Service:Create(owner, info, props)
		owner.last = t
		t.played = true
	end
	animate(table.remove(queue, 1), { FieldOfView = table.remove(queue, 1) })
	local first = owner.last.info .. ":" .. owner.last.props.FieldOfView
	animate(string.rep("x", #queue), { FieldOfView = table.remove(queue, 1) })
	local second = owner.last.info .. ":" .. owner.last.props.FieldOfView
	animate(table.remove(queue, 1), { FieldOfView = #queue })
	return first, second, owner.last.info .. ":" .. owner.last.props.FieldOfView
end
print(queued({}, { "quad", 75, 56, "sine" }))
print(table.concat(created, ","))

local trace = {}
local Log = {}
function Log.note(tag, value)
	table.insert(trace, tag)
	return value
end
local note = Log.note

local function swapped(info, props)
	local t = Service:Create(trace, props, info)
	table.insert(trace, "swap:" .. tostring(t.info) .. ":" .. tostring(t.props))
end

local function twice(value)
	local t = Service:Create(trace, value, value)
	table.insert(trace, "twice:" .. tostring(t.info))
end

local function maybe(flag, value)
	if flag then
		table.insert(trace, "maybe:" .. tostring(value))
	end
	table.insert(trace, "after")
end

swapped(note("a", "first"), note("b", "second"))
twice(note("t", 74))
maybe(#trace > 2, note("m", 1))
maybe(#trace < 2, note("n", 2))
print(table.concat(trace, " "))

-- Negatives:
-- `swapped` reads its parameters in the other order: SSA keeps the first
-- argument in a temp, which the rebuilt call takes in ahead of the moved
-- one. `twice` reads its parameter twice and `maybe` only on one path:
-- their arguments are evaluated where Luau evaluated them. `Log.note` is a
-- table function, which Luau never inlines, so each such argument stays a
-- call; the trace shows the order.
