-- `current:report(change())`: NAMECALL reads the register `current` after
-- the argument reassigned it. A helper taking it as a parameter
-- (`helper(current)`) captured the old object; the first-read proof walked a
-- statement call's receiver before its arguments.
local function make(name)
    return {
        name = name,
        report = function(self, v)
            print("report", self.name, v)
        end,
    }
end
local current = make("old")
local function change()
    current = make("new")
    return 1
end
local function helper(object)
    object:report(change())
end
current:report(change())
return helper
