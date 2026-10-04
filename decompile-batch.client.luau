--!nocheck
-- decompile-batch.client.luau
--
-- Executor-side client for the Tovek decompiler server's BATCH + RAW features.
--
-- The simple client (decompile.client.luau) base64-encodes each script and POSTs
-- them one-at-a-time as SynSaveInstance walks the game — hundreds of round-trips,
-- each paying base64 encode/decode. This client instead:
--   1. pre-walks every LocalScript/ModuleScript/Script up front,
--   2. reads bytecode into one chunk bounded by script count and estimated bytes,
--      then sends it via POST /decompile/batch,
--   3. caches the results by Instance, then installs getgenv().decompile to answer
--      from the cache (no HTTP during the SynSaveInstance pass),
--   4. falls back to a single POST for any cache miss.
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
local CHUNK_SIZE = 250 -- maximum scripts per HTTP request.
local MAX_CHUNK_BYTES = 8 * 1024 * 1024 -- also bound bytes; count alone is insufficient.
local MAX_SCRIPT_BYTES = 16 * 1024 * 1024
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

    local response = post({
        Url = BATCH_URL,
        Method = "POST",
        Headers = { ["Content-Type"] = contentType },
        Body = body,
    })

    if response.StatusCode and response.StatusCode >= 400 then
        if response.StatusCode == 413 and #chunk > 1 then
            -- A server can impose a smaller aggregate envelope. Split the
            -- current bounded chunk; no input needs to be read a second time.
            local middle = math.floor(#chunk / 2)
            local left, right = {}, {}
            for i, entry in ipairs(chunk) do
                local destination = if i <= middle then left else right
                destination[#destination + 1] = entry
            end
            local leftOk = sendChunk(left, cache)
            local rightOk = sendChunk(right, cache)
            return leftOk and rightOk
        end
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
            end
            -- Leave failed rows uncached: a temporary CPU admission failure
            -- must be eligible for the existing single-script fallback.
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
        response = post({
            Url = RAW_URL,
            Method = "POST",
            Headers = {
                ["Content-Type"] = "application/octet-stream",
                ["X-Script-Name"] = inst:GetFullName(),
            },
            Body = bc,
        })
    else
        response = post({
            Url = SINGLE_URL,
            Method = "POST",
            Headers = { ["X-Script-Name"] = inst:GetFullName() },
            Body = crypt.base64.encode(bc),
        })
    end

    if response.StatusCode and response.StatusCode >= 400 then
        return ("-- decompile failed (HTTP %d): %s"):format(response.StatusCode, tostring(response.Body))
    end
    return response.Body, true
end

-- Keep one byte-budgeted chunk of bytecode at a time. Completed source remains
-- cached for SynSaveInstance, but the full game's bytecode is never retained.
local cache = {}
local chunk, chunkBytes = {}, 12
local function flush()
    if #chunk == 0 then return end
    sendChunk(chunk, cache)
    chunk, chunkBytes = {}, 12
end
for _, inst in ipairs(collectScripts()) do
    local ok, bc = pcall(getscriptbytecode, inst)
    if ok and type(bc) == "string" and #bc > 0 then
        if #bc > MAX_SCRIPT_BYTES then
            cache[inst] = "-- decompile failed: bytecode exceeds the server's per-script limit"
            continue
        end
        local name = inst:GetFullName()
        local wireBytes = if USE_RAW then #bc + #name + 8 else math.ceil(#bc / 3) * 4 + #name * 6 + 64
        if #chunk > 0 and (#chunk >= CHUNK_SIZE or chunkBytes + wireBytes > MAX_CHUNK_BYTES) then flush() end
        chunk[#chunk + 1] = { instance = inst, name = name, bytecode = bc }
        chunkBytes += wireBytes
        if chunkBytes >= MAX_CHUNK_BYTES then flush() end
    end
end
flush()

-- The hook SynSaveInstance calls per script: answer from cache, else single POST.
getgenv().decompile = function(script_instance)
    local source = cache[script_instance]
    if source then return source end
    local success
    source, success = singleDecompile(script_instance)
    if success then cache[script_instance] = source end
    return source
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
