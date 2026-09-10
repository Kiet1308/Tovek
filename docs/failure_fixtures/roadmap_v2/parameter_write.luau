return function(input, choose)
    local before = function() return input end
    if choose then
        input += 10
    else
        input -= 10
    end
    local result = input * 2
    return result, before()
end
