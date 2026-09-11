local function describe(value)
    if type(value) == "table" then
        return "table:" .. tostring(rawget(value, "tag")) .. ":" .. tostring(rawget(value, 1))
    end
    if type(value) == "function" then return "function" end
    return tostring(value)
end

for flagIndex = 1, 4 do
    local flag = if flagIndex == 1 then nil elseif flagIndex == 2 then false elseif flagIndex == 3 then true else 0
    for _, throwing in {"", "yes", "no", "mutate", "prefix", "tail", "lookup", "condition", "done", "body", "step", "start", "limit", "argument", "receiver", "callback"} do
        local trace, conditionCount, doneCount = {}, 0, 0
        local function event(label)
            table.insert(trace, label)
            if label == throwing then error(label, 0) end
        end
        local function emit(label, value, mutate)
            event(label)
            if mutate then mutate() end
            if label == "condition" then
                conditionCount += 1
                value = conditionCount < 4
            elseif label == "done" then
                doneCount += 1
                value = doneCount >= 3
            end
            return value, "extra"
        end
        local function callback(...)
            event("callback")
            table.insert(trace, "call:old:" .. select("#", ...))
            return "old", ...
        end
        local function otherCallback(...)
            event("callback")
            table.insert(trace, "call:new:" .. select("#", ...))
            return "new", ...
        end
        local function object(tag, value)
            return setmetatable({tag = tag, Value = value}, {
                __index = function(self, key)
                    event("lookup")
                    table.insert(trace, "lookup:" .. tag .. ":" .. key)
                    return function(receiver, ...)
                        table.insert(trace, "method:" .. receiver.tag .. ":" .. select("#", ...))
                        return receiver.tag, ...
                    end
                end,
            })
        end
        local old, new = object("old", 11), object("new", 22)
        local function iterator(_, control)
            local nextIndex = (control or 0) + 1
            if nextIndex <= 2 then return nextIndex, nextIndex * 10 end
        end
        local function receiver()
            event("receiver")
            return old
        end
        local result = table.pack(pcall(f, flag, emit, 7, callback, old, otherCallback, new, iterator, receiver --[[IR_EXTRA_ARGS]]
        ))
        local fields = {tostring(flagIndex), throwing, tostring(result.n)}
        for i = 1, result.n do table.insert(fields, describe(result[i])) end
        print(table.concat(fields, "\t"))
        print(table.concat(trace, "|"))
    end
end
