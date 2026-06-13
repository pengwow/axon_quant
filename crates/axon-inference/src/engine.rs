use std::any::Any;
use std::path::Path;

use crate::error::InferenceError;
use crate::error::{Action, Observation};

pub trait InferenceEngine: Send + Sync {
    fn load(&mut self, path: &Path) -> Result<(), InferenceError>;
    fn infer(&self, observation: &Observation) -> Result<Action, InferenceError>;
    fn infer_batch(&self, observations: &[Observation]) -> Result<Vec<Action>, InferenceError>;
    fn replace_session(&mut self, new_session: Box<dyn Any>) -> Result<(), InferenceError>;
}
