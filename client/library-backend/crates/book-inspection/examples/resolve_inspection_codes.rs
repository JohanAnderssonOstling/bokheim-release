//! Resolve newline-separated extracted LCC candidates for read-only audits.
use std::io::BufRead;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in std::io::stdin().lock().lines() {
        let code = line?;
        println!("{}", serde_json::json!({"code":code,"usable":book_inspection::evidence::usable_lcc(&code),"paths":subject_projection::unified_lcc_subject_paths(&code)}));
    }
    Ok(())
}
