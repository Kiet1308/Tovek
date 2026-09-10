return function(flag)
    local onDestroy
    local disconnected = 0
    local function once(callback)
        onDestroy = callback
        return { Disconnect = function() disconnected += 1 end }
    end
    local connection = nil
    if not flag then
        connection = once(function() connection = nil end)
    end
    local function cleanup()
        if connection then connection:Disconnect(); connection = nil end
    end
    return function() if onDestroy then onDestroy() end; cleanup(); return disconnected end, cleanup
end
