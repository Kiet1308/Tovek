return {
    direct = function(target)
        local widget = require("Widget")
        target.Widget = widget
        return target
    end,
    nested = function(target)
        local widget = require("Widget")
        target.Nested.Widget = widget
        return target
    end,
    intervening = function(target, step)
        local widget = require("Widget")
        step()
        target.Widget = widget
        return target
    end,
    captured = function(target, bind)
        local components = target
        bind(function(nextTarget)
            components = nextTarget
        end)
        local widget = require("Widget")
        components.Widget = widget
        return components
    end,
    shared = function(target)
        local widget = require("Widget")
        target.Widget = widget
        return widget
    end,
    constructor = function()
        local components = {}
        local widget = require("Widget")
        components.Widget = widget
        return components
    end,
    callable = function()
        local factory = require("Widget")
        return factory(7)
    end,
}
