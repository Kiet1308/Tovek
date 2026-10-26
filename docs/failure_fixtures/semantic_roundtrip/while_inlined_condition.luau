-- Rebuilt head tests must run once per iteration, including after continue.
local trace = {}
local function alive(humanoid)
    return humanoid ~= nil and humanoid.Health > 0
end
local function run(humanoid)
    while alive(humanoid) do
        humanoid.Health -= 1
        if humanoid.Health % 2 == 0 then continue end
        trace[#trace + 1] = humanoid.Health
    end
    return humanoid and humanoid.Health
end
for _, health in {0, 1, 4} do print("alive", run({Health = health})) end
print("nil", run(nil))

local probes, step = 0, 0
local function done()
    probes += 1
    return step >= 5
end
while true do
    if done() then break end
    step += 1
    if step % 2 == 0 then continue end
    trace[#trace + 1] = step
end
print("probes", probes, step)

-- A preceding effect cannot move across the test; NaN cannot reverse < to >=.
local value = 0 / 0
local iterations = 0
while true do
    if value < 0 then break end
    iterations += 1
    if iterations == 2 then break end
end
print("nan", iterations)
local visits = 0
while true do
    visits += 1
    if visits == 2 then break end
    trace[#trace + 1] = visits
end
print("visits", visits, table.concat(trace, ","))
