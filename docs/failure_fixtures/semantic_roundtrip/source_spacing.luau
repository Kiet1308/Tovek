-- Gaps between call statements survive available compiler line metadata.
local function spacing()
    print("first")
    print("adjacent")

    print("separated")
    print("last")
end

local function nested()
    return function()
        print("nested-first")

        print("nested-separated")
    end
end

spacing()
nested()()
return spacing, nested
