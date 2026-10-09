-- Value helpers whose whole body is `return V` (expression mode): Luau's -O2
-- copy is `V` itself, evaluated in place inside the caller's statement. The
-- de-inliner rebuilds the call there: a field store taking in the temp Luau
-- made for an argument the body reads three times (`cfg.Lifetime =
-- jit(cfg.Lifetime)`), and the store's address temp too (`cfg.Micro.Amplitude
-- = jit(cfg.Micro.Amplitude)`: Luau evaluates the base before the value); a
-- zero-parameter helper reading an outer local in a constructor item, a last
-- argument (it returns exactly one value) and a method receiver; a helper
-- whose root is a call in a single store, an index base and a non-last return
-- value.
-- `shout`, a call root whose copy was cut to one result, keeps the cut:
-- `(shout(s))`.
-- Negative: `stamp` reads `counter` after the temp `t` was computed by hand,
-- and computing `t` changes `counter`: moving `hooks.bump()` into the call
-- would read `counter` before it changes, so the temp stays.
-- Negative: `M.echo` writes `string.rep(s, 2, "!")` by hand as the value of
-- a `return`, which takes all of its results. Luau never inlines `shout`
-- there (a helper returning a call is not single-valued), so that is no copy
-- and stays as written.
local Players = { LocalPlayer = { Character = { Name = "Hero", Size = 3 } } }
local LocalPlayer = Players.LocalPlayer
local rng = { scale = 0.5 }
function rng:Next(low, high)
	return (low + high) * self.scale + 1
end
local variation = 0.25

local function jit(p)
	return p and (p * (1 + rng:Next(-variation, variation)) or p)
end

local function GetCharacter()
	return LocalPlayer and LocalPlayer.Character
end

local function toPair(t)
	return table.concat(t, ":", 1, 2)
end

local function shout(s)
	return string.rep(s, 2, "!")
end

local counter = 0
local hooks = {}
function hooks.bump()
	counter += 1
	return counter
end

local M = {}

function M.vary(cfg)
	cfg.Lifetime = jit(cfg.Lifetime)
	cfg.Micro.Amplitude = jit(cfg.Micro.Amplitude)
	return cfg.Lifetime, cfg.Micro.Amplitude
end

function M.ignore(list)
	local holder = { GetCharacter() }
	table.insert(list, GetCharacter())
	return #list, holder[1].Name, (GetCharacter() or {}).Size
end

function M.pairs(keys, part)
	part.Pair = toPair(keys[1])
	print(toPair(keys[2]):upper())
	return toPair(keys[3]), keys[3][3]
end

function M.stamp()
	local t = hooks.bump()
	return counter, t and (t * (1 + rng:Next(-variation, variation)) or t)
end

function M.shout(s)
	return s, (string.rep(s, 2, "!"))
end

function M.echo(s, loud)
	if loud then
		return string.rep(s, 2, "!")
	end
	return s
end

print(M.vary({ Lifetime = 2, Micro = { Amplitude = 4 } }))
print(M.vary({ Micro = {} }))
print(M.ignore({}))
local part = {}
print(M.pairs({ { "a", "b" }, { "c", "d" }, { "e", "f", "g" } }, part), part.Pair)
print(M.stamp())
print(select("#", M.shout("hi")), M.shout("x"))
print(M.echo("a", true), M.echo("b", false))
