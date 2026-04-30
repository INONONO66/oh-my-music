use std::env;
use std::time::Duration;

use omm_audio::source::{GlicolSource, TestToneSource};
use omm_audio::{AudioRuntime, AudioRuntimeConfig};
use omm_engine::cpal_output::CpalOutput;

const SAMPLE_RATE: u32 = 48_000;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        return Ok(());
    }

    match args[1].as_str() {
        "test-tone" => run_test_tone().await?,
        "glicol" => {
            if args.len() < 3 {
                eprintln!("Error: glicol command requires code argument");
                eprintln!("Usage: omm-engine glicol \"<glicol code>\"");
                std::process::exit(1);
            }
            run_glicol(&args[2]).await?;
        }
        unknown => {
            eprintln!("Unknown command: {unknown}");
            print_usage();
            std::process::exit(1);
        }
    }
    Ok(())
}

fn print_usage() {
    println!("oh-my-music engine");
    println!();
    println!("Usage:");
    println!("  omm-engine test-tone");
    println!("  omm-engine glicol \"<glicol code>\"");
    println!();
    println!("Examples:");
    println!("  omm-engine test-tone");
    println!("  omm-engine glicol \"out: sin 440 >> mul 0.1\"");
}

async fn run_test_tone() -> anyhow::Result<()> {
    println!("Starting test tone (440Hz, 5 seconds)...");

    let mut runtime = AudioRuntime::new(AudioRuntimeConfig {
        sample_rate: SAMPLE_RATE,
    });
    let source = Box::new(TestToneSource::new(440.0, SAMPLE_RATE));
    runtime.add_source(source);

    let _output = CpalOutput::new(runtime)?;
    println!("Audio stream started. Press Ctrl-C to stop.");

    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(5)) => {
            println!("5 seconds elapsed, stopping.");
        }
        _ = tokio::signal::ctrl_c() => {
            println!("Received Ctrl-C, stopping.");
        }
    }
    Ok(())
}

async fn run_glicol(code: &str) -> anyhow::Result<()> {
    println!("Loading Glicol code: {code}");

    let mut runtime = AudioRuntime::new(AudioRuntimeConfig {
        sample_rate: SAMPLE_RATE,
    });
    let mut source = GlicolSource::new(SAMPLE_RATE);

    if let Err(e) = source.load_code(code) {
        eprintln!("Failed to load Glicol code: {e}");
        std::process::exit(1);
    }
    println!("Glicol code loaded successfully.");

    runtime.add_source(Box::new(source));
    let _output = CpalOutput::new(runtime)?;
    println!("Audio stream started. Press Ctrl-C to stop.");

    tokio::signal::ctrl_c().await?;
    println!("Received Ctrl-C, stopping.");
    Ok(())
}
