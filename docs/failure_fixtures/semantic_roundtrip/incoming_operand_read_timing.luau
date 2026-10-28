-- Incoming reads are eager; O2 can inline their helper into a mutable owning register.
-- Every original input profile remains the oracle, including source optimization differences.

local function constructor(key, after, invoke)
    local function change() key = after; return 9 end
    local function inner() return { [key] = invoke(change) } end
    local result = inner()
    return result.before, result.after
end

local function savedConstructor(key, after, invoke)
    local function change() key = after; return 9 end
    local function inner() local saved = key; return { [saved] = invoke(change) } end
    local result = inner()
    return result.before, result.after
end

local function storeKey(before, after, invoke)
    local key = before
    local result = {}
    local function change() key = after; return 9 end
    local function inner()
        result[key] = invoke(change)
    end
    inner()
    return result.before, result.after
end

local function savedStoreKey(before, after, invoke)
    local key = before
    local result = {}
    local function change() key = after; return 9 end
    local function inner()
        local saved = key
        result[saved] = invoke(change)
    end
    inner()
    return result.before, result.after
end

local function storeReceiver(before, after, invoke)
    local first, second = {}, {}
    local target = first
    local function change() target = second; return 9 end
    local function inner()
        target.field = invoke(change)
    end
    inner()
    return first.field, second.field
end

local function savedStoreReceiver(before, after, invoke)
    local first, second = {}, {}
    local target = first
    local function change() target = second; return 9 end
    local function inner()
        local saved = target
        saved.field = invoke(change)
    end
    inner()
    return first.field, second.field
end

local function storeBoth(before, after, invoke)
    local first, second = {}, {}
    local target, key = first, before
    local function change() target = second; key = after; return 9 end
    local function inner() target[key] = invoke(change) end
    inner()
    return first.before, first.after, second.before, second.after
end

local function indexReceiver(before, after, invoke)
    local target = {field = 10}
    local replacement = {field = 20}
    local function change() target = replacement; return "field" end
    local function inner() return target[invoke(change)] end
    return inner()
end

local function add(before, after, invoke)
    local value = before
    local function change() value = after; return 2 end
    local function inner() return value + invoke(change) end
    return inner()
end

local function equal(before, after, invoke)
    local value = before
    local function change() value = after; return 20 end
    local function inner() return value == invoke(change) end
    return inner()
end

local function less(before, after, invoke)
    local value = before
    local function change() value = after; return 20 end
    local function inner() return value < invoke(change) end
    return inner()
end

local function fastcall(before, after, invoke)
    local value = before
    local function change() value = after; return 15 end
    local function inner() return math.max(value, (invoke(change))) end
    return inner()
end

local function fastcallAlias(before, after, invoke)
    local value = before
    local function change() value = after; return 15 end
    local maximum = math.max
    local function inner() return maximum(value, (invoke(change))) end
    return inner()
end

local function namecall(before, after, invoke)
    local function method(self, value) return self.name, value end
    local receiver = {name = "before", method = method}
    local replacement = {name = "after", method = method}
    local function change() receiver = replacement; return 9 end
    local function inner() return receiver:method(invoke(change)) end
    local result = inner()
    return result
end

local function invoke(callback) return callback() end
for _, case in {

    { "constructor", constructor, "before", "after" },

    { "savedConstructor", savedConstructor, "before", "after" },

    { "storeKey", storeKey, "before", "after" },

    { "savedStoreKey", savedStoreKey, "before", "after" },

    { "storeReceiver", storeReceiver, "before", "after" },

    { "savedStoreReceiver", savedStoreReceiver, "before", "after" },

    { "storeBoth", storeBoth, "before", "after" },

    { "indexReceiver", indexReceiver, "before", "after" },

    { "add", add, 10, 20 },

    { "equal", equal, 10, 20 },

    { "less", less, 10, 20 },

    { "fastcall", fastcall, 10, 20 },

    { "fastcallAlias", fastcallAlias, 10, 20 },

    { "namecall", namecall, "before", "after" },

} do
    print(case[1], pcall(case[2], case[3], case[4], invoke))
end
