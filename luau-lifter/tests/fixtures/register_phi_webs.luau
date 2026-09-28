-- One bytecode register per source variable: its phi web must stay one local.
local M = {}

function M.rescan(player, data, effects)
    local cached = {}
    local lastRescan = 0
    while player.Parent ~= nil do
        local now = os.clock()
        if now - lastRescan >= 10 then
            lastRescan = now
            cached = data:GetData()
        end
        for i = 1, #cached do
            effects.Update(cached[i])
        end
    end
end

function M.clamp(x, minValue, maxValue, velocity)
    if x < minValue then
        x = minValue
        velocity = 0
    elseif x > maxValue then
        x = maxValue
        velocity = 0
    end
    return x, velocity
end

return M
