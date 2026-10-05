-- 179 parameters and calls whose arguments hold conditionals: written as
-- branch statements, each conditional needs the arguments evaluated before
-- it in locals of their own, more than the 200 Luau allows even with every
-- local sharing storage. The last retry writes the branches as `if`
-- expressions, which declare none, instead of refusing. Found by the
-- register fuzzer.
local G, H = 1, 2
local T = setmetatable({k = 1, f = 3}, {__index = function() return 0 end})
local O = {m = function(self, ...) return select("#", ...) end}
local function F(...) return select("#", ...) end
local function N(x) return type(x) == "number" and x or 1 end
local function S(x) return tostring(x) end
local function E() local done = false return function() if done then return nil end done = true return 1 end end
local function C() return false end
local function body(p0, p1, p2, p3, p4, p5, p6, p7, p8, p9, p10, p11, p12, p13, p14, p15, p16, p17, p18, p19, p20, p21, p22, p23, p24, p25, p26, p27, p28, p29, p30, p31, p32, p33, p34, p35, p36, p37, p38, p39, p40, p41, p42, p43, p44, p45, p46, p47, p48, p49, p50, p51, p52, p53, p54, p55, p56, p57, p58, p59, p60, p61, p62, p63, p64, p65, p66, p67, p68, p69, p70, p71, p72, p73, p74, p75, p76, p77, p78, p79, p80, p81, p82, p83, p84, p85, p86, p87, p88, p89, p90, p91, p92, p93, p94, p95, p96, p97, p98, p99, p100, p101, p102, p103, p104, p105, p106, p107, p108, p109, p110, p111, p112, p113, p114, p115, p116, p117, p118, p119, p120, p121, p122, p123, p124, p125, p126, p127, p128, p129, p130, p131, p132, p133, p134, p135, p136, p137, p138, p139, p140, p141, p142, p143, p144, p145, p146, p147, p148, p149, p150, p151, p152, p153, p154, p155, p156, p157, p158, p159, p160, p161, p162, p163, p164, p165, p166, p167, p168, p169, p170, p171, p172, p173, p174, p175, p176, p177, p178)
    for w0 = 1, 2 do
        if (F((not O:m(p37, p116, p142, p169.f)), p20.f, (-N(O:m(p160, H, p68.f, p31, p2))), T[T[2]], T[p24], (if p37 then (p157.f == true) else 2), (if T[2] then T[p12.f] else T[p115]), p12.f, {(-N(T.k)), T[nil], #S(0), #S(p110), (N(p54) - N(p158)), p157, T.k, (p159 or p64), p131, (-N(p61)), F(), #S(p123), math.pi, p103, true, p83, F(), p123, k0 = F(), k1 = F(), k2 = nil, k3 = #S(p1)}, F(false, p150), F(math.pi, G, p111.f), 0.5, #S(0), T.k, O:m(p150, p171.f, {p165.f, p168.f, -3, p9.f, p75.f, math.pi, p131.f, p34, p133, p71, p128, p147.f, p76, k0 = p67, k1 = math.pi}, F(p168, p126.f)), `a{(N(p81.f) / N(p78))}b{math.pi}`, p61, {(N("s") + N(p71)), (if p109 then p42 else p127), (-N("s")), O:m(), F(7, 1), (not p45.f), F(p163.f), k0 = F(), k1 = F(), k2 = p114}, F(T.k, p65)) and ((N(#S(F("s", w0))) <= N((not #S(p103)))) or (not #S(#S(p101))))) then
            F(nil, (not -0.0), p65, #S(F(7, H, 0, p70)), p41, (N(F(p8, p127.f, p77.f, p93.f)) / N(T[F(p67.f, 2)])), O:m(p126, p10.f, T[(not -0.0)]), `a{O:m(#S(T.k), (p86 ~= p145.f), O:m(p13.f, p51, p58, p122.f, p175, p158), p129)}b{#S(false)}`, (N(p33) <= N(p142.f)), p54, p173)
            local v180 = F(p41, F({p127, p18, true, p156.f, true, 1, p146.f, p20.f, p71, p145, 7, p141.f, -0.0, math.pi, p172, -0.0, p145, p13.f, G, k0 = p118.f, k1 = p119}, O:m(p122, 7, p56), T[p58], {0, p162, nil, H, p116, p62, true, -0.0, p4.f, k0 = 1e+300, k1 = p109, k2 = p165, k3 = -0.0}, F(), (p165.f or p9.f)), `a{#S((N(p23) <= N(1)))}b{O:m((p123 == p43.f), 0.5, F(p156.f))}`, {(-N((N("s") + N(H)))), (2 ~= (not T.k)), (T[p93.f] == p91), (-N(p103)), ((not math.pi) ~= p4), (if #S(p151) then `a{p163.f}b{p3}` else math.pi), k0 = T[p177], k1 = F(-3, p38, p121.f, H)}, (-N((T[2] and O:m(nil, p35, "s", p8)))), (-N(`a{(N(p110.f) < N(math.pi))}b{O:m(p114, p121, p68, p58, "s", 0)}`)), p0.f, (not p157))
            F(p158.f)
        end
        local v180, v181, v182 = p84.f
        F(p142.f)
    end
    local v179, v180, v181 = #S(2), p47, (-N((-N((F(-0.0, 1) or p134)))))
    p177 = p57
end
local args = {}
for index = 1, 179 do
    args[index] = setmetatable({f = index}, {__index = function() return 0 end})
end
print(pcall(body, table.unpack(args)))
