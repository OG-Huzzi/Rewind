use clap::Parser;

fn main() {
    let cli = rewind::cli::Cli::parse();
    match rewind::cli::run(cli) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("rewind: {error}");
            std::process::exit(1);
        }
    }
}
