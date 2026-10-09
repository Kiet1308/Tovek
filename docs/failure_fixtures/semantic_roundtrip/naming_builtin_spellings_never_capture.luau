-- Locals named from builtin-spelled fields (`.Type` reads as `kind`,
-- `.Error` as `err`, `.Shared` as `Shared`) and an alias of a builtin never
-- capture a later read of the global. `local type = type` is the alias's
-- spelling only where the global is not read again inside its region.
local record = { Type = "fire", Error = "bad", Shared = "folder", Next = 3, String = "s" }

local function describe(r)
	local kind = r.Type
	local err = r.Error
	local folder = r.Shared
	local following = r.Next
	local text = r.String
	return type(kind) .. ":" .. kind .. ":" .. tostring(err) .. ":" .. folder .. ":"
		.. tostring(next({ following })) .. ":" .. string.upper(text)
end

local type = type
local function check(value)
	return type(value)
end

local alias = select
local function count(...)
	return alias("#", ...), select(2, ...)
end

print(describe(record))
print(check(1), check("s"), check(nil))
print(count(4, 5, 6))
