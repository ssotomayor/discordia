use clap::Parser;
use discordia_music::{
    probe::{MetadataProbe, Status},
    source::Source,
};
use std::{error::Error, process::ExitCode};

#[derive(Parser)]
#[command(about = "Validate music links; optionally probe public metadata. Does not play audio.")]
struct Args {
    /// Contact the provider's fixed metadata endpoint, without music account credentials.
    #[arg(long)]
    probe: bool,
    #[arg(required = true, num_args = 1..=32)]
    links: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("Music source validation failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<bool, Box<dyn Error>> {
    let sources: Vec<_> = args
        .links
        .iter()
        .map(|link| Source::parse(link))
        .collect::<Result<_, _>>()?;
    if !args.probe {
        println!("{}", serde_json::to_string_pretty(&sources)?);
        return Ok(true);
    }
    let probe =
        MetadataProbe::new().map_err(|_| "could not initialize the HTTPS metadata client")?;
    let mut available = true;
    let mut reports = Vec::new();
    for source in sources {
        let report = probe.inspect(source).await;
        available &= report.status == Status::MetadataAvailable;
        reports.push(report);
    }
    println!("{}", serde_json::to_string_pretty(&reports)?);
    Ok(available)
}
