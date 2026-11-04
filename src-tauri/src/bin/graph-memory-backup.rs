use graph_memory_lib::{backup_format::FORMAT_VERSION, backup_io};
use serde_json::json;
use std::path::PathBuf;

const USAGE: &str = "graph-memory-backup export --data-dir DIR --output NEW_FILE.gmbackup\ngraph-memory-backup verify FILE.gmbackup\ngraph-memory-backup restore --input FILE.gmbackup --data-dir EMPTY_DIR [--dry-run]\nNo default data directory. Export never overwrites a file. Restore requires an empty target directory.";

enum Command {
    Export(PathBuf, PathBuf),
    Verify(PathBuf),
    Restore(PathBuf, PathBuf, bool),
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
        Some("restore") => {
            let (mut input, mut data, mut dry_run) = (None, None, false);
            while let Some(arg) = args.next() {
                match arg.to_str() {
                    Some("--dry-run") if !dry_run => dry_run = true,
                    Some("--input") if input.is_none() => {
                        input = Some(PathBuf::from(
                            args.next()
                                .filter(|p| !p.is_empty())
                                .ok_or("restore option needs a path")?,
                        ));
                    }
                    Some("--data-dir") if data.is_none() => {
                        data = Some(PathBuf::from(
                            args.next()
                                .filter(|p| !p.is_empty())
                                .ok_or("restore option needs a path")?,
                        ));
                    }
                    _ => return Err("unknown or duplicate restore option".into()),
                }
            }
            Ok(Command::Restore(
                input.ok_or("--input is required")?,
                data.ok_or("--data-dir is required")?,
                dry_run,
            ))
        }
        _ => Err("expected export, verify, restore or --help".into()),
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
    let (operation, dry_run, result) = match command {
        Command::Help => {
            println!("{USAGE}");
            return;
        }
        Command::Export(data, output) => ("export", None, backup_io::export_workspace(&data, &output)),
        Command::Verify(path) => ("verify", None, backup_io::verify_file(&path)),
        Command::Restore(input, data, dry_run) => (
            "restore",
            Some(dry_run),
            backup_io::restore_workspace(&input, &data, dry_run),
        ),
    };
    match result {
        Ok(counts) => match dry_run {
            Some(dry_run) => println!(
                "{}",
                json!({"status":"ok", "operation":operation, "format_version":FORMAT_VERSION,
                    "counts":counts, "dry_run":dry_run})
            ),
            None => println!(
                "{}",
                json!({"status":"ok", "operation":operation, "format_version":FORMAT_VERSION,"counts":counts})
            ),
        },
        Err(error) => {
            eprintln!("{operation} failed: {error}");
            std::process::exit(1);
        }
    }
}
