use crate::{
    Cli,
    error::{CliError, ErrorCategory},
};
use clap::CommandFactory;
use clap_complete::{Shell, generate};
use std::io::{self, Write};

pub fn completions(shell: Shell) -> Result<(), CliError> {
    let stdout = io::stdout();
    write_completions(&mut stdout.lock(), shell)
}

pub fn manpage() -> Result<(), CliError> {
    let stdout = io::stdout();
    write_manpage(&mut stdout.lock())
}

fn write_completions(output: &mut impl Write, shell: Shell) -> Result<(), CliError> {
    let mut command = Cli::command();
    let mut buffer = Vec::new();
    generate(shell, &mut command, "hypercarry", &mut buffer);
    output.write_all(&buffer).map_err(output_error)
}

fn output_error(error: io::Error) -> CliError {
    CliError::with_source(
        ErrorCategory::Output,
        "could not write generated CLI artifact",
        error,
    )
}

fn write_manpage(output: &mut impl Write) -> Result<(), CliError> {
    clap_mangen::Man::new(Cli::command())
        .render(output)
        .map_err(|error| {
            CliError::with_source(
                ErrorCategory::Output,
                "could not render hypercarry man page",
                error,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_completions_include_operational_commands() {
        let mut output = Vec::new();
        write_completions(&mut output, Shell::Bash).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("_hypercarry"));
        assert!(output.contains("predict"));
        assert!(output.contains("basis"));
    }

    #[test]
    fn manpage_has_name_synopsis_and_exit_contract() {
        let mut output = Vec::new();
        write_manpage(&mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(".TH hypercarry"));
        assert!(output.contains("SYNOPSIS"));
        assert!(output.contains("130 cancelled"));
    }
}
