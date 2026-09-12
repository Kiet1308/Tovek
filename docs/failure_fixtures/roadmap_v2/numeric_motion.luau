return {
    checked = function(first)
        for i = first, 2 do
            local value = i * 2 + 1
            touch()
            consume(value)
        end
    end,
    mutated = function()
        for i = 1, 2 do
            local value = i + 1
            i = touch(i)
            consume(value, i)
        end
    end,
    captured = function()
        for i = 1, 2 do
            local function replace(value) i = value end
            local value = i + 1
            touch(replace)
            consume(value, i)
        end
    end,
    typed = function(input: number)
        local value = input + 1
        touch()
        consume(value)
    end,
    division = function()
        for i = 0, 1 do
            local value = (i - i) / (i - i)
            touch()
            consume(value)
        end
    end,
}
