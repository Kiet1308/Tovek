local function check(mode, condition, primary, fallback)
    local events = {}
    local result = f(mode, condition, primary, fallback, function(value)
        table.insert(events, tostring(value))
    end)
    print(mode .. ":" .. tostring(condition) .. ":" .. tostring(result) .. ":" .. table.concat(events, ","))
end

check("select", true, nil, false)
check("select", false, 7, false)
check("select", true, 17, 23)
check("select", false, 17, 23)
check("normalize", false, nil, {1, 2, 3})
check("normalize", false, 2, {1, 2, 3})
check("normalize", false, 0, {1, 2, 3})
check("normalize", true, false, {1})
check("capture", true, 7, false)
check("capture", false, 7, false)
