use mesh_sim::run_reference_scenario;

#[tokio::main]
async fn main() {
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
