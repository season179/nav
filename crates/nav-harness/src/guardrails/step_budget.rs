//! Main-loop step budget primitives.

use crate::sessions::ModelTurn;

pub const DEFAULT_STEP_BUDGET: usize = 80;
pub const FINAL_STEP_SUMMARY_MESSAGE: &str = concat!(
    "Tools are now disabled. Do not call any tools. ",
    "Respond with a text-only progress summary: summarize what you completed, ",
    "what remains, and any blockers."
);

/// Tracks the main-loop budget where one step is one model call plus its tool batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepBudget {
    max_steps: usize,
    completed_steps: usize,
}

impl Default for StepBudget {
    fn default() -> Self {
        Self::with_max_steps(DEFAULT_STEP_BUDGET)
    }
}

impl StepBudget {
    /// Build a step budget with a custom limit.
    ///
    /// A zero limit is clamped to one so the loop can still run the final
    /// text-only summary step.
    pub fn with_max_steps(max_steps: usize) -> Self {
        Self {
            max_steps: max_steps.max(1),
            completed_steps: 0,
        }
    }

    pub fn max_steps(&self) -> usize {
        self.max_steps
    }

    /// Reserve the next model-call-plus-tool-batch step.
    ///
    /// The returned decision disables tools and carries the synthetic summary
    /// prompt when this reservation is the final allowed step.
    pub fn next_step(&mut self) -> Result<StepBudgetDecision, StepBudgetError> {
        if self.completed_steps >= self.max_steps {
            return Err(StepBudgetError::Exhausted {
                max_steps: self.max_steps,
            });
        }

        let step_number = self.completed_steps + 1;
        self.completed_steps = step_number;
        let final_step = step_number == self.max_steps;
        let synthetic_message = if final_step {
            Some(ModelTurn::assistant_text(FINAL_STEP_SUMMARY_MESSAGE))
        } else {
            None
        };

        Ok(StepBudgetDecision {
            step_number,
            tools_enabled: !final_step,
            synthetic_message,
        })
    }
}

/// The loop wiring decision for a reserved step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepBudgetDecision {
    step_number: usize,
    tools_enabled: bool,
    synthetic_message: Option<ModelTurn>,
}

impl StepBudgetDecision {
    pub fn step_number(&self) -> usize {
        self.step_number
    }

    pub fn tools_enabled(&self) -> bool {
        self.tools_enabled
    }

    pub fn synthetic_message(&self) -> Option<&ModelTurn> {
        self.synthetic_message.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepBudgetError {
    Exhausted { max_steps: usize },
}
