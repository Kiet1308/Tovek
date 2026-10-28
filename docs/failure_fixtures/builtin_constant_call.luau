local capturedFirst, capturedSecond = 1, 2
return {
    direct = function()
        return math.max(1, 2)
    end,
    alias = function()
        local maximum = math.max
        return maximum(1, 2)
    end,
    named = function()
        local first, second = 1, 2
        return math.max(first, second)
    end,
    captured = function()
        return math.max(capturedFirst, capturedSecond)
    end,
    capturedNegation = function()
        return math.max(-capturedFirst, 2)
    end,
    capturedAddition = function()
        return math.max(capturedFirst + 1, 2)
    end,
    namedAddition = function()
        local first = 1
        return math.max(first + 1, 2)
    end,
    namedTuple = function()
        local first, second = 1, 2
        local maximum = math.max
        return maximum(first + second, 2)
    end,
    constants = function()
        return math.max(3.141592653589793, 1 / 0, 1 / 3)
    end,
    effectful = function()
        local cell = 0
        local function argument()
            cell += 1
            return 1
        end
        local maximum = math.max
        local result = maximum(argument(), 2)
        return result, cell
    end,
    parameters = function(first, second)
        return math.max(first, second)
    end,
}
