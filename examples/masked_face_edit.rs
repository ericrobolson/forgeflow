//! Reproducible, headless test of the same masked redraw backend as the native editor.
//! Run: cargo run --example masked_face_edit -- path/to/job.json
use forgeflow::{face_edit, image_gen::{EditEvent, LocalImageBackend}, image_models, picker::{FaceEditSettings, Pick}};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
struct Job {
    target: PathBuf,
    reference: PathBuf,
    output: PathBuf,
    target_box: [u32; 4],
    reference_box: [u32; 4],
    settings: FaceEditSettings,
    resolution: u32,
    seed: i64,
    #[serde(default = "default_lightning")]
    lightning: bool,
}

fn default_lightning() -> bool { true }

fn main() -> Result<(), String> {
    forgeflow::reaper::initialize();
    let spec = std::env::args().nth(1).ok_or("usage: masked_face_edit job.json")?;
    let job: Job = serde_json::from_slice(&std::fs::read(spec).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let project = forgeflow::project::Project { root };
    let work = forgeflow::edit_workspace::EditWorkspace::new(&project)?;
    let target = image::open(&job.target).map_err(|e| e.to_string())?;
    let pick = Pick { width: target.width(), height: target.height(), region: Some(job.target_box), ..Default::default() };
    let expanded = face_edit::context_pick(&pick, &job.settings)?;
    let crop = work.path.join("target-context.png");
    forgeflow::picker::crop(&job.target, &expanded, &crop)?;
    let reference_size = image::image_dimensions(&job.reference).map_err(|e| e.to_string())?;
    let reference_pick = Pick { width: reference_size.0, height: reference_size.1, region: Some(job.reference_box), ..Default::default() };
    let reference = work.path.join("reference-face.png");
    forgeflow::picker::crop_face(&job.reference, &reference_pick, &reference, 0.25, 512)?;
    let mask = work.path.join("target-mask.png");
    face_edit::context_mask(&pick, &job.settings)?.save(&mask).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&job.output).map_err(|e| e.to_string())?;
    for path in [&crop, &reference, &mask] {
        std::fs::copy(path, job.output.join(path.file_name().unwrap())).map_err(|e| e.to_string())?;
    }
    let mut model = image_models::resolve_installed("qwen-image-edit-2511")?;
    model.width = job.resolution;
    model.height = job.resolution;
    model.steps = job.settings.steps;
    if !job.lightning { model.files.retain(|file| file.flag != image_models::LORA_FLAG); }
    model.extra_args.extend(["--mmap".into(), "--max-vram".into(), "20".into(), "--threads".into(), "4".into(), "--seed".into(), job.seed.to_string()]);
    let generated = LocalImageBackend::new(model)?.inpaint_streaming(
        &job.settings.instruction, &[crop, reference], &mask, job.settings.strength,
        &work.path, job.settings.live_preview, &mut |event| match event {
            EditEvent::Progress(message) => println!("{message}"),
            EditEvent::Preview(path) => println!("Live preview: {}", path.display()),
        },
    );
    let log = work.path.join("renderer.log");
    if log.is_file() { std::fs::copy(log, job.output.join("renderer.log")).map_err(|e| e.to_string())?; }
    let generated = generated?;
    let generated = image::load_from_memory(&generated.bytes).map_err(|e| e.to_string())?;
    generated.save(job.output.join("generated-context.png")).map_err(|e| e.to_string())?;
    let result = face_edit::composite_context(&target, &generated, &pick, &job.settings)?;
    result.save(job.output.join("result.png")).map_err(|e| e.to_string())?;
    // Check the final preservation guarantee against the actual generation, including
    // the oval's corners and all pixels outside the selected rectangle.
    let [left, top, _, _] = expanded.region.unwrap();
    let allowed = face_edit::context_mask(&pick, &job.settings)?;
    let original = target.to_rgba8();
    let result = result.to_rgba8();
    let mut changed_inside = 0usize;
    let mut changed_outside = 0usize;
    for (x, y, pixel) in result.enumerate_pixels() {
        let inside = x.checked_sub(left).zip(y.checked_sub(top)).is_some_and(|(x,y)| x < allowed.width() && y < allowed.height() && allowed[(x,y)][0] != 0);
        if *pixel != original[(x,y)] {
            if inside { changed_inside += 1; } else { changed_outside += 1; }
        }
    }
    let report = serde_json::json!({"changed_inside_mask": changed_inside, "changed_outside_mask": changed_outside, "strength": job.settings.strength, "seed": job.seed, "resolution": job.resolution, "lightning": job.lightning, "steps": job.settings.steps});
    std::fs::write(job.output.join("verification.json"), serde_json::to_vec_pretty(&report).unwrap()).map_err(|e| e.to_string())?;
    println!("{report}");
    if changed_outside != 0 { return Err("pixels outside the mask changed".into()); }
    if changed_inside == 0 { return Err("generation did not change any masked pixels".into()); }
    Ok(())
}
