mod cli;
use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    match cli::run(cli::Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
