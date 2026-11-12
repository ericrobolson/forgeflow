//! After selecting a trained LoRA, compare strength 0 and 1 with identical seeds.
//! Run from the test project: cargo run --example lora_smoke -- <review-directory>
use forgeflow::{config::Config, image_gen::{ImageBackend, LocalImageBackend}, image_models};
use std::path::PathBuf;

fn main() -> Result<(), String> {
    forgeflow::reaper::initialize();
    let output = PathBuf::from(std::env::args().nth(1).ok_or("usage: lora_smoke review-directory")?);
    let prompt = std::env::args().nth(2).unwrap_or_else(|| "a studio photograph, soft natural lighting".into());
    // A selector lets a long comparison be resumed one sample at a time.
    let sample = std::env::args().nth(3);
    if sample.as_deref().is_some_and(|s| s != "baseline" && s != "adapter") { return Err("sample must be baseline or adapter".into()); }
    std::fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let config = Config::load(&std::env::current_dir().map_err(|e| e.to_string())?.join("_forgeflow"))?;
    let selected = config.lora.ok_or("select a trained dataset with forgeflow lora first")?;
    let mut model = image_models::resolve_installed(&selected.base_model)?;
    model.steps = 4;
    model.width = 512;
    model.height = 512;
    model.extra_args.extend(["--seed".into(), "42".into(), "--threads".into(), "8".into(), "--tensor-type-rules".into(), "^model\\.diffusion_model\\.=q4_K".into()]);
    for (name, strength) in [("baseline", 0.0), ("adapter", 1.0)] {
        if sample.as_deref().is_some_and(|sample| sample != name) { continue; }
        let mut adapter = selected.clone(); adapter.strength = strength;
        let backend = LocalImageBackend::new(model.clone())?.with_lora(adapter)?;
        let image = backend.generate(&prompt, &mut |message| println!("{name}: {message}"))?;
        forgeflow::image_gen::save_image(&output.join(format!("{name}.png")), &image)?;
    }
    if !output.join("baseline.png").is_file() || !output.join("adapter.png").is_file() {
        println!("Sample saved; render the other sample in the same directory to compare.");
        return Ok(());
    }
    let baseline = image::open(output.join("baseline.png")).map_err(|e| e.to_string())?.to_rgb8();
    let adapter = image::open(output.join("adapter.png")).map_err(|e| e.to_string())?.to_rgb8();
    if baseline.dimensions() != adapter.dimensions() { return Err("sample dimensions differ".into()); }
    let changed = baseline.pixels().zip(adapter.pixels()).filter(|(a,b)| a != b).count();
    let report = serde_json::json!({"seed":42,"steps":4,"resolution":512,"changed_pixels":changed,"total_pixels":baseline.width()*baseline.height(),"note":"A short pipeline check, not a trained-likeness evaluation"});
    std::fs::write(output.join("comparison.json"), serde_json::to_vec_pretty(&report).unwrap()).map_err(|e| e.to_string())?;
    println!("{report}");
    if changed == 0 { return Err("adapter produced no observable pixel changes; inspect renderer logs".into()); }
    Ok(())
}
