-- Curried UI factories, dynamic property keys and a final multret child.
-- The event trace checks evaluation order, including failing table stores.
local trace = {}
local function mark(label, value)
    table.insert(trace, label)
    return value
end

local children = {}
local scope = {}
function scope:New(name)
    mark("new:" .. name)
    return function(props)
        mark("apply:" .. name)
        return props, nil, "tail:" .. name
    end, "unused factory result"
end
local function key()
    return mark("key", children)
end

local result = {
    mark("first", 1),
    Header = mark("header", 2),
    scope:New("Frame")({
        Size = mark("size", 3),
        [key()] = { (scope:New("Child")({ Value = mark("value", 4) })) },
    }),
}
assert(result[1] == 1 and result.Header == 2)
assert(result[2].Size == 3 and result[2][children][1].Value == 4)
assert(result[3] == nil and result[4] == "tail:Frame")
print("tree", table.concat(trace, ","))

-- A snapshot of a mutable capture must stay before the callback.
local mutableKey = "before"
local function mutate()
    mutableKey = "after"
end
local props = {}
local snapshot = mutableKey
mutate()
props[snapshot] = 7
assert(props.before == 7 and props.after == nil)

local function failKey(bad)
    trace = {}
    local ok = pcall(function()
        local t = { mark("before", 1) }
        t[mark("bad-key", bad)] = mark("bad-value", 2)
        t.After = mark("after", 3)
        return t
    end)
    assert(not ok)
    print("failure", table.concat(trace, ","))
end
failKey(nil)
failKey(0 / 0)

trace = {}
local replaced = { first = nil, second = mark("second", 2) }
replaced.first = mark("first", 1)
assert(replaced.first == 1 and replaced.second == 2)
print("replace", table.concat(trace, ","))

-- Numeric keys and the nil in the final multret must overwrite old slots.
trace = {}
local function tail()
    mark("tail")
    return nil, 20, nil, 40
end
local overlap = { mark("array", 1), [2] = mark("old", 9), tail() }
assert(overlap[1] == 1 and overlap[2] == nil and overlap[3] == 20)
assert(overlap[4] == nil and overlap[5] == 40)
print("overlap", table.concat(trace, ","))

-- The factory call is unconditional even when its later use is skipped.
trace = {}
local handle = scope:New("Skipped")
local enabled = false
local skipped = enabled and handle({})
assert(skipped == false)
print("conditional", table.concat(trace, ","))

-- An observable partial table cannot be folded into its declaration.
local captured
local function readPartial()
    assert(captured and captured.first == 1 and captured.last == nil)
    return 2
end
captured = { first = 1 }
captured.last = readPartial()
assert(captured.last == 2)
print("ui-setlist-ok")
