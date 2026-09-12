local function describe(input, level)
    local depth = level or 0
    local out = {}
    if typeof(input) == "table" then
        for _, element in ipairs(input) do
            table.insert(out, (" "):rep(depth + 1))
            table.insert(out, describe(element, depth + 1))
        end
    elseif typeof(input) == "string" then
        table.insert(out, string.format("%q", input))
    else
        table.insert(out, tostring(input))
    end
    return table.concat(out, "")
end

return {
    direct = function(codec, ...)
        local result = table.pack(...)
        for i, value in ipairs(result) do
            if type(value) == "table" then result[i] = codec.Serialize(value) end
        end
        return result
    end,
    indirect = function(codec, ...)
        local result = table.pack(...)
        for i, value in ipairs(result) do
            if type(value) == "table" then
                local converted = codec.Deserialize(value)
                codec.Observe(converted)
                result[i] = converted
            end
        end
        return result
    end,
    describe = describe,
    nouns = function(parts)
        local result = {}
        for i, part in ipairs(parts) do result[i] = part end
        return result
    end,
    grow = function(bufferValue, requestedBytes)
        local newCapacity = requestedBytes // 0.7272727272727273
        local result = buffer.create(newCapacity)
        buffer.copy(result, 0, bufferValue)
        return result
    end,
    mutableCount = function(level, replacement)
        local depth = level or 0
        depth = replacement
        return (" "):rep(depth + 1)
    end
}
