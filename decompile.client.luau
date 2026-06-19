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

getgenv().decompile = function(script_instance)
    local bytecode = getscriptbytecode(script_instance)
    local encoded = crypt.base64.encode(bytecode)

    local response = httpRequest({
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
