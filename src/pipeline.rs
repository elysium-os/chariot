use std::{cell::RefCell, rc::Rc};

use anyhow::{Context, Result};

use crate::{ChariotContext, config::ConfigRecipeId};

pub struct Pipeline {
    context: Rc<ChariotContext>,

    invalidated_recipes: RefCell<Vec<ConfigRecipeId>>,
    attempted_recipes: RefCell<Vec<ConfigRecipeId>>,
}

impl Pipeline {
    pub fn new(context: Rc<ChariotContext>) -> Pipeline {
        Pipeline {
            context,
            invalidated_recipes: RefCell::new(Vec::new()),
            attempted_recipes: RefCell::new(Vec::new()),
        }
    }

    pub fn invalidate_recipe(&self, recipe_id: ConfigRecipeId) -> Result<()> {
        self.invalidated_recipes.borrow_mut().push(recipe_id);
        self.context.recipe_invalidate(recipe_id)
    }

    pub fn execute(self) -> Result<()> {
        self.invalidated_recipes.borrow_mut().dedup();

        for recipe_id in self.invalidated_recipes.borrow().iter() {
            let recipe = &self.context.config.recipes[recipe_id];
            if self.attempted_recipes.borrow().contains(&recipe.id) {
                continue;
            }

            self.context
                .recipe_process(Vec::new(), &mut self.attempted_recipes.borrow_mut(), &self.invalidated_recipes.borrow(), recipe.id)
                .with_context(|| format!("Failed to process recipe `{}`", recipe))?;
        }

        Ok(())
    }
}
