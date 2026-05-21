use std::env;
use std::time::Duration;

use cpal::traits::StreamTrait;
use omm_audio::source::{GlicolSource, MicSource, MusicalTestSource};
use omm_audio::{run_timeline_dj_demo, AudioRuntime, AudioRuntimeConfig, RtCommand};
use omm_engine::cpal_io::CpalIo;
use omm_protocol::SourceId;
use ringbuf::traits::Consumer;

const SAMPLE_RATE: u32 = 48_000;
const MIX_DEMO_DURATION_SECS: u64 = 5;
const MIC_MONITOR_DURATION_SECS: u64 = 5;
const MIC_TEST_DURATION_SECS: u64 = 3;
const FEATURE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MIX_DEMO_GAIN_RAMP_FRAMES: u32 = 4_800;
const MIC_MONITOR_GAIN_DB: f32 = 18.0;
const MIX_DEMO_GLICOL_CODE: &str = "out: sin 110 >> mul 0.1";

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
        "mix-demo" => run_mix_demo().await?,
        "timeline-demo" => run_timeline_demo()?,
        "mic-test" => run_mic_test().await?,
        "mic-monitor" => run_mic_monitor().await?,
        unknown => {
            eprintln!("Unknown command: {unknown}");
            print_usage();
            std::process::exit(1);
        }
    }
    Ok(())
}

fn print_usage() {
    print!("{}", usage_text());
}

fn usage_text() -> String {
    let mut text = String::new();
    text.push_str("oh-my-music engine\n\n");
    text.push_str("Usage:\n");
    text.push_str("  omm-engine test-tone\n");
    text.push_str("  omm-engine glicol \"<glicol code>\"\n");
    text.push_str("  omm-engine mix-demo\n");
    text.push_str("  omm-engine timeline-demo\n");
    text.push_str("  omm-engine mic-test\n\n");
    text.push_str("  omm-engine mic-monitor\n\n");
    text.push_str("Examples:\n");
    text.push_str("  omm-engine test-tone\n");
    text.push_str("  omm-engine glicol \"out: sin 440 >> mul 0.1\"\n");
    text.push_str("  omm-engine mix-demo\n");
    text.push_str("  omm-engine timeline-demo\n");
    text.push_str("  omm-engine mic-test\n");
    text.push_str("  omm-engine mic-monitor\n");
    text
}

async fn run_test_tone() -> anyhow::Result<()> {
    println!("Starting musical test sound (5 seconds)...");

    let (mut runtime, _queue, _features) = AudioRuntime::new(AudioRuntimeConfig {
        sample_rate: SAMPLE_RATE,
    });
    let source = Box::new(MusicalTestSource::new(SAMPLE_RATE));
    runtime.add_channel(SourceId::Glicol, source)?;

    let _io = CpalIo::new(runtime, None)?;
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

    let (mut runtime, _queue, _features) = AudioRuntime::new(AudioRuntimeConfig {
        sample_rate: SAMPLE_RATE,
    });
    let mut source = GlicolSource::new(SAMPLE_RATE);

    if let Err(e) = source.load_code(code) {
        eprintln!("Failed to load Glicol code: {e}");
        std::process::exit(1);
    }
    println!("Glicol code loaded successfully.");

    runtime.add_channel(SourceId::Glicol, Box::new(source))?;
    let _io = CpalIo::new(runtime, None)?;
    println!("Audio stream started. Press Ctrl-C to stop.");

    tokio::signal::ctrl_c().await?;
    println!("Received Ctrl-C, stopping.");
    Ok(())
}

async fn run_mix_demo() -> anyhow::Result<()> {
    println!("Starting mix-demo (Mic + Glicol + musical test source, ~5 seconds)...");

    let (mut runtime, mut command_queue, mut features) = AudioRuntime::new(AudioRuntimeConfig {
        sample_rate: SAMPLE_RATE,
    });

    let mic_stream = build_mic_channel(&mut runtime);

    let mut glicol = GlicolSource::new(SAMPLE_RATE);
    if let Err(err) = glicol.load_code(MIX_DEMO_GLICOL_CODE) {
        eprintln!("Glicol code load failed: {err}");
        std::process::exit(1);
    }
    runtime.add_channel(SourceId::Glicol, Box::new(glicol))?;

    runtime.add_channel(
        SourceId::Player,
        Box::new(MusicalTestSource::new(SAMPLE_RATE)),
    )?;

    if mic_stream.is_some() {
        let _ = command_queue.enqueue(RtCommand::SetChannelGainDb {
            source_id: SourceId::Mic,
            db: -3.0,
            ramp_frames: MIX_DEMO_GAIN_RAMP_FRAMES,
        });
    }
    let _ = command_queue.enqueue(RtCommand::SetChannelGainDb {
        source_id: SourceId::Glicol,
        db: -6.0,
        ramp_frames: MIX_DEMO_GAIN_RAMP_FRAMES,
    });
    let _ = command_queue.enqueue(RtCommand::SetChannelGainDb {
        source_id: SourceId::Player,
        db: -6.0,
        ramp_frames: MIX_DEMO_GAIN_RAMP_FRAMES,
    });

    let _io = if should_skip_mic_in_demo() {
        eprintln!("Audio output disabled in CI; emitting empty feature snapshots.");
        None
    } else {
        Some(CpalIo::new(runtime, mic_stream)?)
    };
    println!("Audio stream started. Press Ctrl-C to stop early.");

    let total_duration = Duration::from_secs(MIX_DEMO_DURATION_SECS);
    let deadline = tokio::time::sleep(total_duration);
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(deadline);
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut deadline => {
                println!("5 seconds elapsed, stopping.");
                return Ok(());
            }
            _ = &mut ctrl_c => {
                println!("Received Ctrl-C, stopping.");
                return Ok(());
            }
            _ = tokio::time::sleep(FEATURE_POLL_INTERVAL) => {
                let snapshots = features.poll_all();
                let json = serde_json::to_string(&snapshots)?;
                println!("{json}");
            }
        }
    }
}

fn run_timeline_demo() -> anyhow::Result<()> {
    let report = run_timeline_dj_demo(Default::default())?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

async fn run_mic_test() -> anyhow::Result<()> {
    println!("Starting microphone test ({MIC_TEST_DURATION_SECS} seconds)...");

    let (stream, mut consumer, cfg) = CpalIo::build_microphone_stream()?;
    stream.play()?;

    println!(
        "Microphone enabled: {} ch @ {} Hz. Speak or tap near the mic now.",
        cfg.channels, cfg.sample_rate
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(MIC_TEST_DURATION_SECS);
    let mut sample_count = 0_u64;
    let mut peak = 0.0_f32;
    let mut sum_squares = 0.0_f64;

    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;

        while let Some(sample) = consumer.try_pop() {
            sample_count += 1;
            peak = peak.max(sample.abs());
            sum_squares += f64::from(sample) * f64::from(sample);
        }
    }

    let rms = if sample_count == 0 {
        0.0
    } else {
        (sum_squares / sample_count as f64).sqrt() as f32
    };

    println!("Captured {sample_count} samples. peak={peak:.5}, rms={rms:.5}");
    Ok(())
}

async fn run_mic_monitor() -> anyhow::Result<()> {
    println!(
        "Starting microphone monitor ({MIC_MONITOR_GAIN_DB:+.1} dB, {MIC_MONITOR_DURATION_SECS} seconds)..."
    );

    let (mut runtime, mut command_queue, _features) = AudioRuntime::new(AudioRuntimeConfig {
        sample_rate: SAMPLE_RATE,
    });
    let mic_stream = build_mic_channel(&mut runtime);

    if mic_stream.is_none() {
        anyhow::bail!("microphone is unavailable");
    }

    let _ = command_queue.enqueue(RtCommand::SetChannelGainDb {
        source_id: SourceId::Mic,
        db: MIC_MONITOR_GAIN_DB,
        ramp_frames: MIX_DEMO_GAIN_RAMP_FRAMES,
    });

    let _io = CpalIo::new(runtime, mic_stream)?;
    println!("Monitoring microphone to output. Keep volume low to avoid feedback.");

    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(MIC_MONITOR_DURATION_SECS)) => {
            println!("{MIC_MONITOR_DURATION_SECS} seconds elapsed, stopping.");
        }
        _ = tokio::signal::ctrl_c() => {
            println!("Received Ctrl-C, stopping.");
        }
    }

    Ok(())
}

fn build_mic_channel(runtime: &mut AudioRuntime) -> Option<cpal::Stream> {
    if should_skip_mic_in_demo() {
        eprintln!("Microphone unavailable; continuing without mic: disabled in CI");
        return None;
    }

    let (stream, consumer, cfg) = match CpalIo::build_microphone_stream() {
        Ok(parts) => parts,
        Err(err) => {
            eprintln!("Microphone unavailable; continuing without mic: {err}");
            return None;
        }
    };

    let mic = match MicSource::new(
        consumer,
        usize::from(cfg.channels),
        cfg.sample_rate,
        SAMPLE_RATE,
    ) {
        Ok(mic) => mic,
        Err(err) => {
            eprintln!("Microphone source init failed; continuing without mic: {err}");
            return None;
        }
    };

    if let Err(err) = runtime.add_channel(SourceId::Mic, Box::new(mic)) {
        eprintln!("Failed to attach mic channel; continuing without mic: {err}");
        return None;
    }

    println!(
        "Microphone enabled: {} ch @ {} Hz.",
        cfg.channels, cfg.sample_rate
    );
    Some(stream)
}

fn should_skip_mic_in_demo() -> bool {
    is_ci_env_value(env::var("CI").ok().as_deref())
}

fn is_ci_env_value(value: Option<&str>) -> bool {
    matches!(value, Some("true") | Some("1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_text_lists_all_subcommands() {
        let text = usage_text();
        assert!(
            text.contains("test-tone"),
            "usage missing test-tone: {text}"
        );
        assert!(text.contains("glicol"), "usage missing glicol: {text}");
        assert!(text.contains("mix-demo"), "usage missing mix-demo: {text}");
        assert!(
            text.contains("timeline-demo"),
            "usage missing timeline-demo: {text}"
        );
        assert!(text.contains("mic-test"), "usage missing mic-test: {text}");
        assert!(
            text.contains("mic-monitor"),
            "usage missing mic-monitor: {text}"
        );
    }

    #[test]
    fn is_ci_env_value_recognizes_true_and_one() {
        assert!(is_ci_env_value(Some("true")));
        assert!(is_ci_env_value(Some("1")));
        assert!(!is_ci_env_value(Some("false")));
        assert!(!is_ci_env_value(Some("0")));
        assert!(!is_ci_env_value(Some("")));
        assert!(!is_ci_env_value(Some("yes")));
        assert!(!is_ci_env_value(None));
    }
}
