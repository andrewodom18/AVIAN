use mesh_sim::{run_reference_scenario, run_visual_scenario};

#[tokio::main]
async fn main() {
    if std::env::args().any(|argument| argument == "--trace") {
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
