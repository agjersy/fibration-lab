//! `present-sandbox`: run one command in the code sandbox (PROTOCOL.md section 9).
//!
//! The Python kernel launches this on Windows; see `wb_sandbox::cli` for the command line.

use std::process::ExitCode;

use wb_sandbox::cli::{self, Cli};
use wb_sandbox::{is_native_isolation, prepare_helper_process, run_inherited, EXIT_HELPER_FAILURE};

fn main() -> ExitCode {
    let spec = match cli::parse(std::env::args_os().skip(1)) {
        Ok(Cli::Run(spec)) => spec,
        Ok(Cli::Help) => {
            print!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Cli::Version) => {
            println!("present-sandbox {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("present-sandbox: {e}");
            eprint!("{}", cli::USAGE);
            return ExitCode::from(EXIT_HELPER_FAILURE as u8);
        }
    };
    if !is_native_isolation() {
        eprintln!("present-sandbox: warning: no isolation on this platform; resource limits only");
    }
    prepare_helper_process();
    match run_inherited(&spec) {
        // `process::exit` rather than `ExitCode`: Windows exit codes use all 32 bits.
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("present-sandbox: {e}");
            ExitCode::from(EXIT_HELPER_FAILURE as u8)
        }
    }
}
