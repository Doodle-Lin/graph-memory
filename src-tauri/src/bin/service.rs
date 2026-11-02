fn main() -> anyhow::Result<()> {
    env_logger::init();
    let mut args = std::env::args().skip(1);
    let mut data_dir = None;
    let mut port = 0;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                data_dir = Some(std::path::PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--data-dir needs a path"))?,
                ))
            }
            "--port" => {
                port = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--port needs a number"))?
                    .parse::<u16>()?
            }
            "--help" | "-h" => {
                println!("graph-memory-service --data-dir PATH [--port PORT]\nExperimental isolated keyword-only API; no model download or maintenance.");
                return Ok(());
            }
            _ => anyhow::bail!("unknown argument: {arg}"),
        }
    }
    let dir = data_dir.ok_or_else(|| {
        anyhow::anyhow!("--data-dir is required; desktop data is never selected automatically")
    })?;
    graph_memory_lib::service::run(&dir, port)
}
