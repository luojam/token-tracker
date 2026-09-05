use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    match token_tracker::run() {
        Ok(report) => {
            if let Err(error) = io::stdout().lock().write_all(report.as_bytes()) {
                eprintln!("token-tracker: could not write report: {error}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("token-tracker: {error}");
            ExitCode::FAILURE
        }
    }
}
