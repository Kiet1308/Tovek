local function encode(value)
    if type(value) == "number" and value ~= value then return "nan" end
    if type(value) == "table" or type(value) == "function" then return type(value) end
    return tostring(value)
end
local function values(...)
    local result = {tostring(select("#", ...))}
    for i = 1, select("#", ...) do table.insert(result, encode(select(i, ...))) end
    return table.concat(result, ":")
end
for variant = 1, 8 do
    for failure = 0, 24 do
        local trace, output = {}, {}
        local count, epoch, reader = 0, 0, nil
        local function event(label)
            count += 1
            table.insert(trace, label)
            if failure == count then error("STOP" .. failure, 0) end
        end
        local input = if variant == 1 then nil elseif variant == 2 then false
            elseif variant == 3 then true elseif variant == 4 then 0
            elseif variant == 5 then "" elseif variant == 6 then {}
            elseif variant == 7 then function() end else 0 / 0
        local api = {}
        api.event = event
        api.next = function(label)
            event(label)
            epoch += 1
            return input, nil, epoch
        end
        api.test = function() event("test"); return variant % 2 == 0 end
        api.keep = function(callback) event("keep"); reader = callback end
        api.check = function(value) event("check:" .. encode(value)); return value end
        local data = setmetatable({}, {__index = function(_, key)
            event("index:" .. key)
            if key == "Y" then
                api.check = function(value) event("changed:" .. encode(value)); return input end
            end
            return if key == "X" then input elseif key == "Y" then true else false
        end})
        local compared = setmetatable({}, {__lt = function() event("compare"); return variant % 2 == 0 end})
        local ok, err = pcall(function()
            table.insert(output, values(f.scalar(input, api)))
            table.insert(output, values(f.conditional(input, api)))
            table.insert(output, values(f.andValue(input, api)))
            table.insert(output, values(f.orValue(input, api)))
            table.insert(output, values(f.andGuard(input, api)))
            table.insert(output, values(f.orGuard(input, api)))
            table.insert(output, values(f.tuple(input, api, false, nil, "tail")))
            table.insert(output, values(f.scalarVararg(input, false, nil, "tail")))
            table.insert(output, values(f.captured(input, api)))
            table.insert(output, values(reader()))
            table.insert(output, values(f.parameter(input, api)))
            table.insert(output, values(f.fields(data, api)))
            table.insert(output, values(f.ordered(compared, compared)))
            table.insert(output, values(f.ordered(0 / 0, 1)))
            table.insert(output, values(f.ordered(1, 0 / 0)))
        end)
        print(variant .. ":" .. failure .. "|" .. tostring(ok)
            .. "|" .. (if ok then "ok" else tostring(err):match("STOP%d+") or "error")
            .. "|" .. table.concat(output, ",") .. "|" .. table.concat(trace, ","))
    end
end
