//! Model-visible coding tools.

use super::support;
use super::{
    CancelFlag, Tool, ToolError, ToolOutput, arg_opt_bool, arg_opt_str, arg_opt_u64, arg_str,
};

mod bash;
mod edit;
mod find;
mod grep;
mod ls;
mod read;
mod write;

pub(super) fn coding_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(read::ReadTool),
        Box::new(bash::BashTool),
        Box::new(edit::EditTool),
        Box::new(write::WriteTool),
        Box::new(grep::GrepTool),
        Box::new(find::FindTool),
        Box::new(ls::LsTool),
    ]
}
