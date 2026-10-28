return function()
    do
        local function probe(value) return (math.abs(value)) end
        print("single", probe(-3))
        print("twice", probe(-3), probe(-4))
    end
    do
        local function probe(value)
            local result = math.abs(value)
            print("called")
            return result
        end
        print("effect", probe(-3), probe(-4))
    end
    do
        local function probe(value)
            if value then return (math.abs(value)) end
            return false
        end
        print("branch", probe(-3), probe(false))
    end
    do
        local function probe(value) return (math.abs(value)) end
        local alias = probe
        print("alias", alias(-3), alias(-4))
    end
    do
        local function probe(value)
            local derived = value + 1
            return (math.abs(derived))
        end
        print("derived", probe(-3), probe(-4))
    end
    do
        local function identity(value) return value end
        local function probe(value) return (math.abs((identity(value)))) end
        print("nested", probe(-3), probe(-4))
    end
end
