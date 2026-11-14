//! AgentChat command-line executable.

#[tokio::main(worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(agentchat_cli::run(std::env::args_os()).await as u8)
}
