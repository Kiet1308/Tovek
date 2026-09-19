return function(value, other, api)
    api.event("start")
    if value == nil then end
    if value == false then end
    if value == "ready" then end
    if value == 2 then end
    api.event("primitive")
    if value == other then end
    api.event("equality")
    if value.field then end
    api.event("lookup")
    if value + 1 then end
    api.event("arithmetic")
    local _ = api.call(value)
    return nil, false, "done"
end
