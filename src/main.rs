use std::process::Command;

use clap::{arg, Parser};
use rlwrap::config::RlwrapConfig;
use rlwrap::Rlwrap;

/// Wrap a program in a input/output prompt
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Prefix of the prompt
    #[clap(short = 'S', long, value_parser, default_value = "> ")]
    substitute_prompt: String,
    /// The program that will be executed
    program: String,
    /// Any arguments to use for that program
    args: Vec<String>,
}

fn main() {
    let args: Args = Args::parse();

    let rlwrap = Rlwrap::setup(RlwrapConfig {
        stop_on_ctrl_c: true,
        prefix: args.substitute_prompt,
    })
    .unwrap();

    match Command::new(args.program).args(&args.args).spawn() {
        Ok(mut child) => {
            child.wait().unwrap();
        }
        Err(e) => {
            println!("Failed to spawn process: {e:?}");
        }
    }

    Rlwrap::stop_gracefully(&rlwrap).unwrap();
}
