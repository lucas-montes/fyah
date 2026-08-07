use std::collections::VecDeque;

use serde::Deserialize;

use crate::context::{AgentContext, Message, Prompt, SessionContext};

pub struct DefaultPromptBuilder;

impl PromptBuilder for DefaultPromptBuilder {
    fn build(&self, _context: &AgentContext) -> Prompt {
        todo!()
    }
}

#[derive(Debug, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PromptBuilderCfg {
    #[default]
    Default,
    Custom(String),
}

impl PromptBuilderCfg {
    pub fn build(&self) -> Box<dyn PromptBuilder> {
        match self {
            PromptBuilderCfg::Default => Box::new(DefaultPromptBuilder),
            PromptBuilderCfg::Custom(_name) => {
                // Here you would implement logic to create a custom prompt builder based on the name
                // For now, we'll just return the default as a placeholder
                Box::new(DefaultPromptBuilder)
            }
        }
    }
}

pub struct DefaultContextBuilder;

impl ContextBuilder for DefaultContextBuilder {
    fn append_to_session(
        &self,
        _session_context: &mut SessionContext,
        _agent_context: AgentContext,
    ) {
        // Implement the default logic to append context to the session
    }

    fn populate_from_session(&self, _session_context: &SessionContext) -> AgentContext {
        // Implement the default logic to populate context from the session
        todo!()
    }

    fn handle_new_messages(&self, _messages: &mut VecDeque<Message>, _context: &mut AgentContext) {
        // Implement the default logic to handle new messages and update the context
    }
}

#[derive(Debug, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContextBuilderCfg {
    #[default]
    Default,
    Custom(String),
}

impl ContextBuilderCfg {
    pub fn build(&self) -> Box<dyn ContextBuilder> {
        match self {
            ContextBuilderCfg::Default => Box::new(DefaultContextBuilder),
            ContextBuilderCfg::Custom(_name) => {
                // Here you would implement logic to create a custom context builder based on the name
                // For now, we'll just return the default as a placeholder
                Box::new(DefaultContextBuilder)
            }
        }
    }
}

pub struct DefaultContextCompaction;

impl ContextCompaction for DefaultContextCompaction {
    fn compact(&self, _context: &mut AgentContext) {}
}

#[derive(Debug, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompactionCfg {
    #[default]
    Default,
    Custom(String),
}

impl ContextCompactionCfg {
    pub fn build(&self) -> Box<dyn ContextCompaction> {
        match self {
            ContextCompactionCfg::Default => Box::new(DefaultContextCompaction),
            ContextCompactionCfg::Custom(_name) => {
                // Here you would implement logic to create a custom context compaction based on the name
                // For now, we'll just return the default as a placeholder
                Box::new(DefaultContextCompaction)
            }
        }
    }
}

pub struct DefaultToolsFilter;

impl<Tool> ToolsFilter<Tool> for DefaultToolsFilter {
    fn filter(&self, _tool_calls: &Vec<Tool>) {}
}

#[derive(Debug, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolsFilterCfg {
    #[default]
    Default,
    Custom(String),
}

impl ToolsFilterCfg {
    pub fn build<Tool>(&self) -> Box<dyn ToolsFilter<Tool>> {
        match self {
            ToolsFilterCfg::Default => Box::new(DefaultToolsFilter),
            ToolsFilterCfg::Custom(_name) => {
                // Here you would implement logic to create a custom tools filter based on the name
                // For now, we'll just return the default as a placeholder
                Box::new(DefaultToolsFilter)
            }
        }
    }
}

pub trait PromptBuilder {
    fn build(&self, context: &AgentContext) -> Prompt;
}

pub trait ContextBuilder {
    fn append_to_session(&self, session_context: &mut SessionContext, agent_context: AgentContext);
    fn populate_from_session(&self, session_context: &SessionContext) -> AgentContext;
    fn handle_new_messages(&self, messages: &mut VecDeque<Message>, context: &mut AgentContext);
}

pub trait ContextCompaction {
    fn compact(&self, context: &mut AgentContext);
}

pub trait ToolsFilter<Tool> {
    fn filter(&self, tool_calls: &Vec<Tool>);
}
