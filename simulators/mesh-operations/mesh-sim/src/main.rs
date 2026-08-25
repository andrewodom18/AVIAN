use std::path::PathBuf;

use mesh_sim::{
    peat_validation::run_bounded_peat_validation, run_reference_scenario, run_visual_scenario,
    validation::run_validation_matrix,
};

#[tokio::main]
async fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .iter()
        .any(|argument| argument == "--validate-peat")
    {
        match run_bounded_peat_validation().await {
            Ok(evidence) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&evidence).expect("serialize PEAT evidence")
                );
                if evidence.iter().any(|item| !item.passed) {
                    std::process::exit(1);
                }
            }
            Err(error) => {
                eprintln!("PEAT validation failed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if arguments.iter().any(|argument| argument == "--validate") {
        let seed = option_value(&arguments, "--seed")
            .and_then(|value| value.parse().ok())
            .unwrap_or(20_260_825);
        let mut report = run_validation_matrix(seed);
        if arguments.iter().any(|argument| argument == "--summary") {
            for scenario in &mut report.scenarios {
                scenario.events.clear();
            }
        }
        let json = serde_json::to_string_pretty(&report).expect("serialize validation report");
        if let Some(output) = option_value(&arguments, "--output").map(PathBuf::from) {
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent).expect("create validation report directory");
            }
            std::fs::write(&output, format!("{json}\n")).expect("write validation report");
            println!("validation report: {}", output.display());
        } else {
            println!("{json}");
        }
        if !report.passed {
            std::process::exit(1);
        }
        return;
    }
    if arguments.iter().any(|argument| argument == "--trace") {
        match run_visual_scenario().await {
            Ok(trace) => println!(
                "{}",
                serde_json::to_string_pretty(&trace).expect("serialize visual trace")
            ),
            Err(error) => {
                eprintln!("visual simulation failed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    match run_reference_scenario().await {
        Ok(report) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("serialize report")
            );
            if !report.passed() {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("simulation failed: {error}");
            std::process::exit(1);
        }
    }
}

fn option_value<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}
