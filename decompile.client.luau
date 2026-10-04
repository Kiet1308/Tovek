--!nocheck
-- decompile.client.luau
--
-- Executor-side client for the Medal Luau decompiler web server (run-server.ps1).
-- Installs a `getgenv().decompile(script)` hook that ships each script's bytecode
-- to the local server, then drives SynSaveInstance in "scripts" mode so every
-- LocalScript / ModuleScript in the game is decompiled through it.
--
-- The server reads the `X-Script-Name` header (matching its API: the value is
-- forwarded to `decompile_bytecode_with_script_name`) so the output is named
-- after the in-game script path — e.g. UI module require() paths resolve nicely.

local SERVER_URL = "http://localhost:3000/decompile"

-- Resolve the executor's HTTP request function (names differ per executor).
local httpRequest = request or http_request or (syn and syn.request) or (http and http.request)
assert(httpRequest, "no executor `request` function found")

-- Retry temporary admission, upload and transport failures with bounded attempts.
local MAX_ATTEMPTS = 5

local function post(options)
    local response
    for attempt = 1, MAX_ATTEMPTS do
        local ok, value = pcall(httpRequest, options)
        response = if ok and type(value) == "table" then value else {
            StatusCode = 599, Body = "request failed: " .. tostring(value), Headers = {},
        }
        local status = response.StatusCode
        if status ~= 408 and status ~= 429 and status ~= 502 and status ~= 503 and status ~= 504 and status ~= 599 then
            break
        end
        if attempt == MAX_ATTEMPTS then break end
        local headers = response.Headers or {}
        local delay = math.max(tonumber(headers["Retry-After"] or headers["retry-after"]) or 0, math.min(8, 2 ^ (attempt - 1)))
        task.wait(delay + math.random() * 0.25)
    end
    return response
end

getgenv().decompile = function(script_instance)
    local bytecode = getscriptbytecode(script_instance)
    local encoded = crypt.base64.encode(bytecode)

    local response = post({
        Url = SERVER_URL,
        Method = "POST",
        Headers = {
            -- The script's full path — the API uses this to name the chunk.
            ["X-Script-Name"] = script_instance:GetFullName(),
        },
        Body = encoded,
    })

    if response.StatusCode and response.StatusCode >= 400 then
        return ("-- decompile failed (HTTP %d): %s"):format(response.StatusCode, response.Body)
    end
    return response.Body
end

-- SynSaveInstance walks the game and calls getgenv().decompile for each script.
local synsaveinstance = loadstring(game:HttpGet("https://raw.githubusercontent.com/luau/SynSaveInstance/main/saveinstance.luau"))()
local Options = {
    SafeMode = true,
    ShutdownWhenDone = true,
    mode = "scripts",
    NilInstances = true,
}
synsaveinstance(Options)
