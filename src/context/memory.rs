pub trait ContextManagement {}

#[derive(Debug, Default)]
pub struct SimpleContext;

impl ContextManagement for SimpleContext {}
