use std::io::{self, Write};

use crate::{
    config::Config,
    device::{self, Device},
    models::{self, CatalogModel, Fit, LocalModel},
    ops::Ops,
    util,
    workflow::{self, Context},
};

/// One numbered entry in the setup menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub id: String,
    pub installed: bool,
    pub size_bytes: u64,
    pub tokens_per_sec: Option<f64>,
    /// Recommendation labels such as "fastest", or "local" for uncatalogued models.
    pub labels: Vec<&'static str>,
}

/// True when nothing is configured: no model selected and no endpoint.
pub fn needs_setup(config: &Config) -> bool {
    config.model.is_none() && config.endpoint.is_none()
}

/// Installed models first, then recommended downloads. When nothing is recommended for
/// this device, the largest standard models that still fit are offered instead.
pub fn choices(
    catalog: &[CatalogModel],
    local: &[LocalModel],
    device: &Device,
    ctx: u64,
    is_installed: &dyn Fn(&CatalogModel) -> bool,
) -> Vec<Choice> {
    let picks = models::recommend(catalog, device, ctx);
    let labels_for = |id: &str| {
        let mut labels = vec![];
        if picks.fastest.as_deref() == Some(id) {
            labels.push("fastest");
        }
        if picks.balanced.as_deref() == Some(id) {
            labels.push("balanced");
        }
        if picks.quality.as_deref() == Some(id) {
            labels.push("best quality");
        }
        labels
    };
    let choice = |model: &CatalogModel, installed: bool| Choice {
        id: model.id.clone(),
        installed,
        size_bytes: model.size_bytes,
        tokens_per_sec: models::estimate_model(model, device, ctx).tokens_per_sec,
        labels: labels_for(&model.id),
    };

    let mut out: Vec<Choice> = catalog
        .iter()
        .filter(|m| is_installed(m))
        .map(|m| choice(m, true))
        .collect();
    out.extend(local.iter().map(|m| Choice {
        id: m.id.clone(),
        installed: true,
        size_bytes: m.size_bytes,
        tokens_per_sec: models::estimate(m.size_bytes, 0, 1.0, device).tokens_per_sec,
        labels: vec!["local"],
    }));

    let mut downloads: Vec<&CatalogModel> = catalog
        .iter()
        .filter(|m| !is_installed(m) && !labels_for(&m.id).is_empty())
        .collect();
    if downloads.is_empty() && picks == models::Picks::default() {
        let mut fitting: Vec<&CatalogModel> = catalog
            .iter()
            .filter(|m| !is_installed(m) && !m.is_uncensored())
            .filter(|m| models::estimate_model(m, device, ctx).fit != Fit::No)
            .collect();
        fitting.sort_by(|a, b| b.params_b.total_cmp(&a.params_b));
        fitting.truncate(3);
        downloads = fitting;
    }
    downloads.sort_by(|a, b| a.size_bytes.cmp(&b.size_bytes));
    out.extend(downloads.into_iter().map(|m| choice(m, false)));
    out
}

fn format_choice(number: usize, choice: &Choice, id_width: usize) -> String {
    let speed = choice
        .tokens_per_sec
        .map(|t| format!("~{t:.0} tok/s"))
        .unwrap_or_default();
    let labels = if choice.labels.is_empty() || choice.labels == ["local"] {
        choice.labels.join(", ")
    } else {
        format!("★ {}", choice.labels.join(", "))
    };
    format!(
        "  {number:>2}) {:<id_width$} {:>6}  {:>11}  {labels}",
        choice.id,
        models::gb(choice.size_bytes),
        speed,
    )
    .trim_end()
    .to_string()
}

enum Answer {
    Pick(String),
    ShowAll,
    Quit,
}

fn parse_answer(input: &str, choices: &[Choice]) -> Result<Answer, String> {
    match input {
        "q" | "quit" => Ok(Answer::Quit),
        "all" | "a" => Ok(Answer::ShowAll),
        "" => Err("enter a number, a model ID, `all`, or `q`".into()),
        _ => match input.parse::<usize>() {
            Ok(n) if (1..=choices.len()).contains(&n) => Ok(Answer::Pick(choices[n - 1].id.clone())),
            Ok(_) => Err(format!("pick a number from 1 to {}", choices.len())),
            Err(_) => Ok(Answer::Pick(input.to_string())),
        },
    }
}

/// Walks the user through choosing, downloading, and selecting a model.
/// Returns false when they quit without choosing one.
pub fn run(context: &mut Context) -> Result<bool, String> {
    let config = Config::load(&context.project.folder())?;
    let device = Device::probe();
    let catalog = models::catalog();
    let free = device::free_disk_bytes(&models::models_dir());
    println!(
        "\nNo model selected yet. Let's pick a local model to chat with.\n{} · {} RAM · ~{} model budget · {} free in {}\n",
        device.chip,
        models::gb(device.ram_bytes),
        models::gb(device.model_budget_bytes()),
        free.map(models::gb).unwrap_or_else(|| "?".into()),
        models::models_dir().display(),
    );
    let choices = choices(
        &catalog,
        &models::uncatalogued_local_models(),
        &device,
        config.ctx,
        &|m| models::install_path(m).exists(),
    );
    let id_width = choices.iter().map(|c| c.id.len()).max().unwrap_or(0);
    let installed = choices.iter().filter(|c| c.installed).count();
    if installed > 0 {
        println!("Installed:");
        for (i, choice) in choices[..installed].iter().enumerate() {
            println!("{}", format_choice(i + 1, choice, id_width));
        }
    } else {
        println!("No models installed yet.");
    }
    if installed < choices.len() {
        println!("\nRecommended for this machine (download):");
        for (i, choice) in choices[installed..].iter().enumerate() {
            println!("{}", format_choice(installed + i + 1, choice, id_width));
        }
    }
    println!(
        "\nTo use Ollama or another OpenAI-compatible server instead, set `endpoint` in {}.",
        context.project.folder().join(crate::config::CONFIG_FILE).display()
    );

    loop {
        print!("\nEnter a number or model ID, `all` for every model, or `q` to quit\n> ");
        io::stdout().flush().ok();
        let line = util::read_line()?;
        if line.is_empty() {
            // End of input.
            return Ok(false);
        }
        let id = match parse_answer(line.trim(), &choices) {
            Ok(Answer::Quit) => return Ok(false),
            Ok(Answer::ShowAll) => {
                println!("\n{}", models::render_table(&device, config.ctx, config.model.as_deref()));
                continue;
            }
            Ok(Answer::Pick(id)) => id,
            Err(error) => {
                println!("{error}");
                continue;
            }
        };
        if models::resolve_installed(&id).is_err() {
            let Some(model) = models::find(&id) else {
                println!("unknown model `{id}`; type `all` to see every model");
                continue;
            };
            if !util::confirmation(&format!("Download {} ({})?", model.name, models::gb(model.size_bytes))) {
                continue;
            }
            if let Err(error) = models::pull(&id, &mut io::stderr()) {
                println!("error: {error}");
                continue;
            }
        }
        match workflow::run_steps(context, &[Ops::StringLiteral(id), Ops::UseModel]) {
            Ok(()) => {
                println!("Switch models any time with /model in chat.\n");
                return Ok(true);
            }
            Err(error) => println!("error: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::GB;
    use std::path::PathBuf;

    fn m4_pro_24gb() -> Device {
        Device {
            chip: "Apple M4 Pro".into(),
            ram_bytes: 24 * 1024 * 1024 * 1024,
            apple_silicon: true,
            bandwidth_gbps: Some(273.0),
        }
    }

    #[test]
    fn fresh_machine_is_offered_the_recommendations_smallest_first() {
        let catalog = models::catalog();
        let device = m4_pro_24gb();
        let picks = models::recommend(&catalog, &device, 8192);
        let choices = choices(&catalog, &[], &device, 8192, &|_| false);
        assert!(choices.iter().all(|c| !c.installed && !c.labels.is_empty()));
        let ids: Vec<_> = choices.iter().map(|c| Some(c.id.clone())).collect();
        for pick in [picks.fastest, picks.balanced, picks.quality] {
            assert!(ids.contains(&pick));
        }
        assert!(choices.windows(2).all(|w| w[0].size_bytes <= w[1].size_bytes));
    }

    #[test]
    fn installed_models_come_first_and_are_not_offered_again() {
        let catalog = models::catalog();
        let device = m4_pro_24gb();
        let picks = models::recommend(&catalog, &device, 8192);
        let fastest = picks.fastest.unwrap();
        let local = LocalModel { id: "my-gguf".into(), path: PathBuf::from("x.gguf"), size_bytes: 3 * GB };
        let choices = choices(&catalog, &[local], &device, 8192, &|m| m.id == fastest);
        assert_eq!(choices[0].id, fastest);
        assert!(choices[0].installed && choices[0].labels.contains(&"fastest"));
        assert_eq!((choices[1].id.as_str(), choices[1].labels.as_slice()), ("my-gguf", ["local"].as_slice()));
        assert_eq!(choices.iter().filter(|c| c.id == fastest).count(), 1);
        assert!(choices[2..].iter().all(|c| !c.installed));
    }

    #[test]
    fn a_small_machine_is_offered_models_that_fit() {
        let device = Device { ram_bytes: 4 * 1024 * 1024 * 1024, ..m4_pro_24gb() };
        let catalog = models::catalog();
        assert_eq!(models::recommend(&catalog, &device, 8192), models::Picks::default());
        let choices = choices(&catalog, &[], &device, 8192, &|_| false);
        assert!(!choices.is_empty() && choices.len() <= 3);
        for choice in &choices {
            let model = models::find(&choice.id).unwrap();
            assert!(!model.is_uncensored());
            assert_ne!(models::estimate_model(&model, &device, 8192).fit, Fit::No);
        }
    }

    #[test]
    fn answers_accept_numbers_ids_and_commands() {
        let choices = choices(&models::catalog(), &[], &m4_pro_24gb(), 8192, &|_| false);
        assert!(matches!(parse_answer("1", &choices), Ok(Answer::Pick(id)) if id == choices[0].id));
        assert!(matches!(parse_answer("qwen3-8b-q4km", &choices), Ok(Answer::Pick(id)) if id == "qwen3-8b-q4km"));
        assert!(matches!(parse_answer("all", &choices), Ok(Answer::ShowAll)));
        assert!(matches!(parse_answer("q", &choices), Ok(Answer::Quit)));
        assert!(parse_answer("0", &choices).is_err());
        assert!(parse_answer("99", &choices).is_err());
    }

    #[test]
    fn setup_runs_only_when_nothing_is_configured() {
        assert!(needs_setup(&Config::default()));
        let with_endpoint = Config { endpoint: Some("http://127.0.0.1:11434/v1".into()), ..Config::default() };
        assert!(!needs_setup(&with_endpoint));
        // A selected model that is missing reports its own error rather than reopening setup.
        let missing = Config { model: Some("not-a-real-model".into()), ..Config::default() };
        assert!(!needs_setup(&missing));
    }
}
