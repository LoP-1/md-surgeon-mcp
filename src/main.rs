pub mod mcp;
pub mod parser;

use anyhow::Result;

fn main() -> Result<()> {
    mcp::run_stdio_server()
}
