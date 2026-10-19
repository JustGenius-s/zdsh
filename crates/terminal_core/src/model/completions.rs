/// A completion result that was produced natively by the shell.
#[derive(Clone, Debug)]
pub struct ShellCompletion {
    pub name: String,
    pub description: Option<String>,
}

/// Enum indicating which field of a [`ShellCompletion`] should be updated.
pub enum ShellCompletionUpdate {
    Description { value: String },
}

impl ShellCompletion {
    pub fn new(name: String) -> Self {
        Self {
            name: name.trim().to_string(),
            description: None,
        }
    }

    pub fn update(&mut self, completion_update: ShellCompletionUpdate) {
        match completion_update {
            ShellCompletionUpdate::Description { value } => {
                if !value.is_empty() {
                    self.description = Some(value.trim().to_string());
                }
            }
        }
    }
}
