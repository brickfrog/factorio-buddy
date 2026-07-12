data:extend({
    {
        type = "custom-input",
        name = "claude-interface-toggle",
        key_sequence = "CONTROL + SHIFT + C",
        action = "lua",
        consuming = "none"
    },
    {
        type = "shortcut",
        name = "claude-interface-toggle",
        order = "z[buddy]",
        action = "lua",
        associated_control_input = "claude-interface-toggle",
        toggleable = true,
        icon = "__base__/graphics/icons/iron-plate.png",
        icon_size = 64,
        small_icon = "__base__/graphics/icons/iron-plate.png",
        small_icon_size = 64
    }
})
