-- Positional event evidence beats where a callback stores its parameter.
local handlers = {}
local function event(name)
    return {Connect = function(_, callback)
        handlers[name] = callback
        return {Disconnect = function() handlers[name] = nil end}
    end}
end
local player = {CharacterAdded = event("character")}
local part = {}
local connection = player.CharacterAdded:Connect(function(value)
    part.Parent = value
    print("character", value.Name, part.Parent == value)
end)
handlers.character({Name = "Avatar"})
connection:Disconnect()
print("disconnected", handlers.character == nil)

-- The suffix in Vector2/Vector3 is part of the type name, not a counter.
local Vector2 = {new = function(x, y) return {X = x, Y = y} end}
local Vector3 = {new = function(x, y, z) return {X = x, Y = y, Z = z} end}
local a = Vector2.new(1, 2)
local b = Vector3.new(3, 4, 5)
local c = Vector3.new(6, 7, 8)
print("vectors", a.X, a.Y, b.X, b.Y, b.Z, c.Z)
