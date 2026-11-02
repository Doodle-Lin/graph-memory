use graph_memory_lib::{backup_format::FORMAT_VERSION, backup_io};
use serde_json::json;
use std::path::PathBuf;

const USAGE: &str = "graph-memory-backup export --data-dir DIR --output NEW_FILE.gmbackup\ngraph-memory-backup verify FILE.gmbackup\nNo default data directory. Export never overwrites a file. Restore is not implemented.";

enum Command {
    Export(PathBuf, PathBuf),
    Verify(PathBuf),
    Help,
}

fn parse() -> Result<Command, String> {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref().and_then(|v| v.to_str()) {
        Some("--help" | "-h") if args.next().is_none() => Ok(Command::Help),
        Some("verify") => {
            let path = args
                .next()
                .filter(|p| !p.is_empty())
                .ok_or("verify needs a backup path")?;
            if args.next().is_some() {
                return Err("unexpected verify arguments".into());
            }
            Ok(Command::Verify(path.into()))
        }
        Some("export") => {
            let (mut data, mut output) = (None, None);
            while let Some(arg) = args.next() {
                let slot = match arg.to_str() {
                    Some("--data-dir") if data.is_none() => &mut data,
                    Some("--output") if output.is_none() => &mut output,
                    _ => return Err("unknown or duplicate export option".into()),
                };
                *slot = Some(PathBuf::from(
                    args.next()
                        .filter(|p| !p.is_empty())
                        .ok_or("export option needs a path")?,
                ));
            }
            Ok(Command::Export(
                data.ok_or("--data-dir is required")?,
                output.ok_or("--output is required")?,
            ))
        }
        _ => Err("expected export, verify or --help".into()),
    }
}

fn main() {
    let command = match parse() {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let (operation, result) = match command {
        Command::Help => {
            println!("{USAGE}");
            return;
        }
        Command::Export(data, output) => ("export", backup_io::export_workspace(&data, &output)),
        Command::Verify(path) => ("verify", backup_io::verify_file(&path)),
    };
    match result {
        Ok(counts) => println!(
            "{}",
            json!({"status":"ok", "operation":operation, "format_version":FORMAT_VERSION,"counts":counts})
        ),
        Err(error) => {
            eprintln!("{operation} failed: {error}");
            std::process::exit(1);
        }
    }
}
