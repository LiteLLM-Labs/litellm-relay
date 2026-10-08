pub mod catalog;
pub mod service;
#[cfg(unix)]
pub mod socket;
#[cfg(unix)]
pub mod stdio;
#[cfg(test)]
pub(crate) mod test_support;
pub mod upstream;
pub mod verdict;

pub const CLIENT_SERVER_NAME: &str = "litellm";
pub const TOOL_NAMES: [&str; 4] = [
    "search_tools",
    "describe_tool",
    "call_tool",
    "activate_server",
];
