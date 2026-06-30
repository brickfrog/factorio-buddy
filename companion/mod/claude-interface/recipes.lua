local M = {}

local function recipe_unlocks(recipe_name)
    local unlocks = {}
    for tech_name, tech in pairs(game.forces.player.technologies) do
        local effects = tech.prototype and tech.prototype.effects or {}
        for _, effect in pairs(effects) do
            if effect.type == "unlock-recipe" and effect.recipe == recipe_name then
                table.insert(unlocks, tech_name)
                break
            end
        end
    end
    table.sort(unlocks)
    return unlocks
end

local function recipe_ingredients(recipe)
    local ingredients = {}
    for _, ingredient in pairs(recipe.ingredients) do
        table.insert(ingredients, {
            type = ingredient.type,
            name = ingredient.name,
            amount = ingredient.amount,
        })
    end
    return ingredients
end

local function recipe_products(recipe)
    local products = {}
    for _, product in pairs(recipe.products) do
        table.insert(products, {
            type = product.type,
            name = product.name,
            amount = product.amount,
            probability = product.probability,
        })
    end
    return products
end

local function recipe_summary(recipe)
    local force_recipe = game.forces.player.recipes[recipe.name]
    return {
        name = recipe.name,
        category = recipe.category,
        energy = recipe.energy,
        enabled = force_recipe and force_recipe.enabled or false,
        unlocked_by = recipe_unlocks(recipe.name),
    }
end

local function recipe_details(recipe)
    local result = recipe_summary(recipe)
    result.ingredients = recipe_ingredients(recipe)
    result.products = recipe_products(recipe)
    return result
end

function M.get_recipe(name)
    local recipe = prototypes.recipe[name]
    if not recipe then
        return {error = "Recipe not found"}
    end
    return recipe_details(recipe)
end

function M.get_recipes_by_category(category)
    local result = {}
    for _, recipe in pairs(prototypes.recipe) do
        if recipe.category == category then
            table.insert(result, recipe_summary(recipe))
        end
    end
    return result
end

function M.get_recipes_for_item(item)
    local result = {}
    for _, recipe in pairs(prototypes.recipe) do
        for _, product in pairs(recipe.products) do
            if product.name == item then
                table.insert(result, recipe_details(recipe))
                break
            end
        end
    end
    return result
end

return M
