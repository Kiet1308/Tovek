--!nocheck
-- decompile-batch.client.luau
--
-- Executor-side client for the Tovek decompiler server's BATCH + RAW features.
--
-- The simple client (decompile.client.luau) base64-encodes each script and POSTs
-- them one-at-a-time as SynSaveInstance walks the game — hundreds of round-trips,
-- each paying base64 encode/decode. This client instead:
--   1. pre-walks every LocalScript/ModuleScript/Script up front,
--   2. reads each one's bytecode and ships them ALL in ONE request per CHUNK_SIZE
--      via POST /decompile/batch (the server decompiles them in parallel),
--   3. caches the results by Instance, then installs getgenv().decompile to answer
--      from the cache (no HTTP during the SynSaveInstance pass),
--   4. falls back to a single POST for any cache miss (so it can never do worse
--      than the simple client).
--
-- RAW vs base64: with USE_RAW = true the batch body is the binary MDB1 framing —
-- raw bytecode, zero base64 on either side. Some executors mangle request bodies
-- that contain NUL/high bytes; if your dump comes back garbled, set USE_RAW =
-- false to use the base64-in-JSON batch instead (still one round-trip per chunk).

local BASE_URL = "http://localhost:3000"
local BATCH_URL = BASE_URL .. "/decompile/batch"
local RAW_URL = BASE_URL .. "/decompile/raw"
local SINGLE_URL = BASE_URL .. "/decompile"

local KEY = 203 -- Roblox client bytecode decode key.
local CHUNK_SIZE = 250 -- scripts per HTTP request (bounds body size + memory).
local USE_RAW = true -- false → base64-in-JSON batch (safe if raw bodies get mangled).

-- Resolve the executor's HTTP request function (names differ per executor).
local httpRequest = request or http_request or (syn and syn.request) or (http and http.request)
assert(httpRequest, "no executor `request` function found")
assert(getscriptbytecode, "executor missing getscriptbytecode")
assert(getgenv, "executor missing getgenv")
assert(string.pack, "Luau string.pack unavailable (update your executor)")
-- The base64 fallback transport needs crypt.base64 — only when USE_RAW = false.
if not USE_RAW then
    assert(crypt and crypt.base64, "USE_RAW = false requires crypt.base64 (set USE_RAW = true)")
end

local HttpService = game:GetService("HttpService")

-- 1) Enumerate every script up front (mirrors SynSaveInstance's NilInstances=true).
local function collectScripts()
    local seen, list = {}, {}
    local function consider(inst)
        if seen[inst] then
            return
        end
        if inst:IsA("LocalScript") or inst:IsA("ModuleScript") or inst:IsA("Script") then
            seen[inst] = true
            list[#list + 1] = inst
        end
    end
    for _, inst in ipairs(game:GetDescendants()) do
        consider(inst)
    end
    if getnilinstances then
        local ok, nils = pcall(getnilinstances)
        if ok and type(nils) == "table" then
            for _, inst in ipairs(nils) do
                consider(inst)
            end
        end
    end
    return list
end

-- 2) Read bytecode for each script (pcall-guarded; skip empty/nil/protected).
local function readEntries(scripts)
    local entries = {}
    for _, inst in ipairs(scripts) do
        local ok, bc = pcall(getscriptbytecode, inst)
        if ok and type(bc) == "string" and #bc > 0 then
            entries[#entries + 1] = { instance = inst, name = inst:GetFullName(), bytecode = bc }
        end
    end
    return entries
end

-- 3a) Build a binary MDB1 batch body (raw bytecode, no base64).
--   header: "MDB1" | version(B) key(B) flags(B) reserved(B) count(I4)  -- all little-endian
--   entry × count: s4(name) s4(bytecode)  -- s4 = u32-LE length prefix + bytes
local function packMdb1(entries)
    local parts = { "MDB1", string.pack("<BBBBI4", 1, KEY, 0, 0, #entries) }
    for _, e in ipairs(entries) do
        parts[#parts + 1] = string.pack("<s4s4", e.name or "", e.bytecode)
    end
    return table.concat(parts)
end

-- 3b) Build a base64-in-JSON batch body (safe fallback transport).
local function packJson(entries)
    local scripts = {}
    for i, e in ipairs(entries) do
        scripts[i] = { script_name = e.name, bytecode = crypt.base64.encode(e.bytecode) }
    end
    return HttpService:JSONEncode({ key = KEY, scripts = scripts })
end

-- 4) Send one chunk and fill cache[instance] = source. Returns true on success.
local function sendChunk(chunk, cache)
    local body, contentType
    if USE_RAW then
        body, contentType = packMdb1(chunk), "application/octet-stream"
    else
        body, contentType = packJson(chunk), "application/json"
    end

    local response = httpRequest({
        Url = BATCH_URL,
        Method = "POST",
        Headers = { ["Content-Type"] = contentType },
        Body = body,
    })

    if response.StatusCode and response.StatusCode >= 400 then
        -- A framing-level failure for the whole chunk; leave entries uncached so
        -- the per-script fallback retries them individually.
        warn(("[decompile-batch] chunk failed (HTTP %d): %s"):format(response.StatusCode, tostring(response.Body)))
        return false
    end

    local ok, decoded = pcall(function()
        return HttpService:JSONDecode(response.Body)
    end)
    if not ok or type(decoded) ~= "table" or type(decoded.results) ~= "table" then
        warn("[decompile-batch] malformed batch response")
        return false
    end

    -- Results are index-aligned and in input order; map each back by Instance.
    for _, result in ipairs(decoded.results) do
        local entry = chunk[(result.index or 0) + 1] -- server index is 0-based.
        if entry then
            if result.ok and result.decompilation then
                cache[entry.instance] = result.decompilation
            else
                cache[entry.instance] = "-- decompile failed: " .. tostring(result.error)
            end
        end
    end
    return true
end

-- Single-script fallback (cache miss / chunk framing failure). Uses the same
-- transport choice as the batch so behavior is consistent.
local function singleDecompile(inst)
    local ok, bc = pcall(getscriptbytecode, inst)
    if not ok or type(bc) ~= "string" or #bc == 0 then
        return "-- failed to get bytecode for " .. inst:GetFullName()
    end

    local response
    if USE_RAW then
        response = httpRequest({
            Url = RAW_URL,
            Method = "POST",
            Headers = {
                ["Content-Type"] = "application/octet-stream",
                ["X-Script-Name"] = inst:GetFullName(),
            },
            Body = bc,
        })
    else
        response = httpRequest({
            Url = SINGLE_URL,
            Method = "POST",
            Headers = { ["X-Script-Name"] = inst:GetFullName() },
            Body = crypt.base64.encode(bc),
        })
    end

    if response.StatusCode and response.StatusCode >= 400 then
        return ("-- decompile failed (HTTP %d): %s"):format(response.StatusCode, tostring(response.Body))
    end
    return response.Body
end

-- Pre-walk, read, and decompile every script in chunked batches.
local cache = {}
local entries = readEntries(collectScripts())
for i = 1, #entries, CHUNK_SIZE do
    local chunk = {}
    for j = i, math.min(i + CHUNK_SIZE - 1, #entries) do
        chunk[#chunk + 1] = entries[j]
    end
    sendChunk(chunk, cache)
end

-- The hook SynSaveInstance calls per script: answer from cache, else single POST.
getgenv().decompile = function(script_instance)
    return cache[script_instance] or singleDecompile(script_instance)
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
