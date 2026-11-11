use std::io::Write;

use crate::{
    agent::{self, Agent},
    agent_loop,
    chat::{self, ChatBackend, Delta, StepOptions},
    config::Config,
    device::Device,
    models,
    type_::{Type, TypeKind},
    util,
    value::Value,
    workflow::*,
};

/// The most lines `reg_view` returns at once.
pub const VIEW_MAX_LINES: usize = 200;
const VIEW_MAX_BYTES: usize = 16 * 1024;
const READ_MAX_BYTES: u64 = 2_000_000;
/// Tokens of the context window kept free for `answer`'s instructions and reply.
const ANSWER_RESERVED_TOKENS: u64 = 1500;
/// A lower bound on characters per token, so trimmed text always fits the context.
const ANSWER_CHARS_PER_TOKEN: u64 = 2;

#[derive(Debug, Clone, PartialEq)]
pub enum Ops {
    SpecifyAgent(agent::Provider),
    StringLiteral(String),
    IntLiteral(i64),
    BoolLiteral(bool),
    /// Pushes a register reference such as `R5`.
    Register(usize),
    AllCliArgs,
    WriteFile,
    /// Reads a line from stdin
    ReadLine,
    AskLlm,
    Print,
    Dup,
    Drop,
    Swap,
    /// `@`: pushes a register's value.
    Fetch,
    /// `!`: pops a value into a register.
    Store,
    ReadFile,
    /// Generate one image and save it below the project root.
    MakeImage,
    /// Edit one or more project images as an instruction asks, saving a new PNG beside the first.
    EditImage,
    ListDirectory,
    /// Files only, without folders.
    ListFiles,
    /// Line-anchored pseudocode summary of a file, cached until its contents change.
    Summarize,
    /// `[ ... ] each`: runs the block once per list item, with the item on the stack.
    Each(Vec<Ops>),
    LocalGet(String, TypeKind),
    AddressOf(String),
    UserCall { name: String, inputs: Vec<TypeKind>, outputs: Vec<TypeKind> },
    RegView,
    RegSearch,
    /// Lines of text containing a pattern.
    Grep,
    /// Streams a model reply to a question about some text.
    Answer,
    /// Explains what a message can ask for.
    Help,
    Add,
    Subtract,
    Multiply,
    Divide,
    RegEdit,
    Emit,
    ShowRegisters,
    Models,
    Pull,
    UseModel,
    ImageModels,
    /// Selects where `make-image` generates: OpenRouter or a local image model.
    UseImage,
    /// Runs the tool-calling agent loop on a task.
    Agent,
    /// `"start" serve` opens the chat to browsers; `"stop" serve` closes it.
    Serve,
    /// Make a video from keyframe images (or none) and a prompt, saved as an `.mp4`.
    MakeVideo,
    /// Set `make-video`'s quality: `draft` or `full`.
    VideoQuality,
    /// Set how long `make-video`'s videos are, in seconds; 0 for the default.
    VideoSeconds,
    /// Upscale a video to 1080p and double its frame rate, saving `NAME-finished.mp4`.
    FinishVideo,
    /// Plan keyframes for a prompt, make them, and animate a draft video through them.
    DraftVideo,
}

impl Ops {
    pub fn token_name(&self) -> String {
        match self { Ops::UserCall { name, .. } => name.clone(), _ => self.name().to_string() }
    }
    /// Every word callable by name, for the compiler's lookup table.
    pub fn named_words() -> Vec<Ops> {
        vec![
            Ops::WriteFile,
            Ops::ReadLine,
            Ops::AskLlm,
            Ops::Print,
            Ops::Dup,
            Ops::Drop,
            Ops::Swap,
            Ops::Fetch,
            Ops::Store,
            Ops::ReadFile,
            Ops::MakeImage,
            Ops::EditImage,
            Ops::ListDirectory,
            Ops::ListFiles,
            Ops::Summarize,
            Ops::RegView,
            Ops::RegSearch,
            Ops::Grep,
            Ops::Answer,
            Ops::Help,
            Ops::Add,
            Ops::Subtract,
            Ops::Multiply,
            Ops::Divide,
            Ops::RegEdit,
            Ops::Emit,
            Ops::ShowRegisters,
            Ops::Models,
            Ops::Pull,
            Ops::UseModel,
            Ops::ImageModels,
            Ops::UseImage,
            Ops::Agent,
            Ops::Serve,
            Ops::MakeVideo,
            Ops::VideoQuality,
            Ops::VideoSeconds,
            Ops::FinishVideo,
            Ops::DraftVideo,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Ops::SpecifyAgent(_) => "specify_agent",
            Ops::StringLiteral(_) => "string literal",
            Ops::IntLiteral(_) => "integer literal",
            Ops::BoolLiteral(_) => "boolean literal",
            Ops::Register(_) => "register",
            Ops::AllCliArgs => "all_cli_args",
            Ops::WriteFile => "write_file",
            Ops::ReadLine => "read_line",
            Ops::AskLlm => "ask",
            Ops::Print => "print",
            Ops::Dup => "dup",
            Ops::Drop => "drop",
            Ops::Swap => "swap",
            Ops::Fetch => "@",
            Ops::Store => "!",
            Ops::ReadFile => "read_file",
            Ops::MakeImage => "make-image",
            Ops::EditImage => "edit-image",
            Ops::Serve => "serve",
            Ops::MakeVideo => "make-video",
            Ops::VideoQuality => "video-quality",
            Ops::VideoSeconds => "video-seconds",
            Ops::FinishVideo => "finish-video",
            Ops::DraftVideo => "draft-video",
            Ops::ListDirectory => "list_directory",
            Ops::ListFiles => "list_files",
            Ops::Summarize => "summarize",
            Ops::Each(_) => "each",
            Ops::LocalGet(_, _) => "local input",
            Ops::AddressOf(_) => "address",
            Ops::UserCall { .. } => "user word",
            Ops::RegView => "reg_view",
            Ops::RegSearch => "reg_search",
            Ops::Grep => "grep",
            Ops::Answer => "answer",
            Ops::Help => "help",
            Ops::Add => "+",
            Ops::Subtract => "-",
            Ops::Multiply => "*",
            Ops::Divide => "/",
            Ops::RegEdit => "reg_edit",
            Ops::Emit => "emit",
            Ops::ShowRegisters => "registers",
            Ops::Models => "models",
            Ops::Pull => "pull",
            Ops::UseModel => "use_model",
            Ops::ImageModels => "image_models",
            Ops::UseImage => "use_image",
            Ops::Agent => "agent",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Ops::ReadFile => "Read a UTF-8 file. The path is relative to the project root.",
            Ops::ListDirectory => {
                "List a folder relative to the project root; use . for the root. Folders end with / and come first."
            }
            Ops::ListFiles => "List the files (not folders) in a folder relative to the project root, alphabetically.",
            Ops::Summarize => "Summarize a file as high-level pseudocode using its real names, with line ranges. Cached until the file changes.",
            Ops::MakeImage => "Generate one image, locally with stable-diffusion.cpp or with OpenRouter, and save it inside the project as a PNG.",
            Ops::MakeVideo => "Make a video locally that passes through the given keyframe images in order (a path, comma-separated paths, a folder of images, or \"\" for none) as the prompt describes, and save it as an .mp4.",
            Ops::VideoQuality => "Set make-video's quality: \"draft\" (512×512, quick, for iterating) or \"full\" (the model's resolution, interpolated and upscaled to 1080p).",
            Ops::DraftVideo => "Make a draft video from a prompt alone: plans and generates start and end keyframes (saved beside it as NAME-key-N.png) so the characters stay consistent, then animates through them at draft quality for video-seconds. Finish it with finish-video.",
            Ops::FinishVideo => "Upscale a video in the project to 1080p and double its frame rate (smoother motion), saving NAME-finished.mp4 beside it; for finishing a draft.",
            Ops::VideoSeconds => "Set how long make-video's videos are, in seconds (0 for one default-length segment per pair of keyframes).",
            Ops::Serve => "Start or stop the web UI, which opens this chat to browsers on this machine and phones on the network (\"start\" prints a QR code; \"stop\" closes it).",
            Ops::EditImage => "Edit a project image (or a list of up to 4, the first being edited) as the instruction asks, locally with stable-diffusion.cpp, and save the result beside it as NAME-edit-N.png.",
            Ops::Each(_) => "Run the [ ] block once per item of a list, with the item on the stack.",
            Ops::RegView => {
                "Show numbered lines of a register's text. start and end are 1-based and inclusive; at most 200 lines."
            }
            Ops::RegSearch => {
                "Find lines in a register's text containing `pattern` (case-insensitive), with line numbers."
            }
            Ops::RegEdit => {
                "Replace exactly one occurrence of `find` in a register's text; the edited text goes to a new register."
            }
            Ops::Store => {
                "Store a value in a register. R0-R3 are durable notes kept across sessions (max 2000 characters)."
            }
            Ops::Emit => "Show a register's full contents to the user, instead of retyping them.",
            Ops::Agent => "Run the local model on a task, letting it call tools.",
            Ops::Grep => "Keep the lines of text that contain a pattern (case-insensitive), numbered.",
            Ops::Answer => {
                "Reply to the user in prose about some context text; use \"\" as the context for general questions."
            }
            Ops::Print => "Show a value to the user.",
            Ops::Help => "Explain what you can ask for.",
            Ops::Add => "Add two integers.",
            Ops::Subtract => "Subtract the top integer from the one below it.",
            Ops::Multiply => "Multiply two integers.",
            Ops::Divide => "Divide the integer below by the top one, rounding toward zero.",
            Ops::Fetch => "Push a register's value.",
            Ops::ShowRegisters => "Show all registers.",
            Ops::Dup => "Copy the top value.",
            Ops::Swap => "Swap the top two values.",
            Ops::Drop => "Discard the top value.",
            Ops::Models => "List local models with size, fit, and speed estimates for this device.",
            Ops::Pull => "Download a catalogued model.",
            Ops::UseModel => "Select the model the agent runs.",
            Ops::ImageModels => "List local image models and show whether make-image uses OpenRouter or a local model.",
            Ops::UseImage => "Select where make-image generates: openrouter, an OpenRouter model ID, or a local image model ID.",
            _ => "",
        }
    }

    pub fn process(&self, context: &mut Context) -> Result<(), String> {
        match self {
            Ops::AllCliArgs => todo!(),
            Ops::WriteFile => {
                let contents = pop(context)?.expect_string()?;
                let path = pop(context)?.expect_string()?;
                let path = context.project.resolve(&path)?;
                std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            Ops::MakeImage => {
                let prompt = pop(context)?.expect_string()?;
                let relative = pop(context)?.expect_string()?;
                let requested_path = std::path::Path::new(relative.trim());
                if !matches!(requested_path.components().next_back(), Some(std::path::Component::Normal(_))) {
                    return Err("image output path must include a filename".into());
                }
                let png_relative = requested_path.with_extension("png");
                let png_relative = png_relative
                    .to_str()
                    .ok_or("image output path is not valid UTF-8")?;
                let path = context.project.resolve(png_relative)?;
                let (config, note) = crate::image_models::effective_generation(&Config::load(&context.project.folder())?);
                if let Some(note) = note {
                    context.progress(note);
                }
                let image = if config.image_backend == crate::config::ImageBackendKind::Local {
                    let model = crate::image_models::resolve_installed(&config.local_image_model)?;
                    with_gpu_room(context, &model, |context| {
                        crate::image_gen::backend(&config)?.generate(&prompt, &mut |line| context.progress(line))
                    })?
                } else {
                    context.progress(format!("Generating with OpenRouter · {}…", config.image_model));
                    crate::image_gen::backend(&config)?.generate(&prompt, &mut |line| context.progress(line))?
                };
                let saved_path = crate::image_gen::save_image(&path, &image)?;
                let saved_relative = saved_path.strip_prefix(&context.project.root)
                    .map_err(|_| "generated image path is outside the project".to_string())?
                    .to_string_lossy()
                    .into_owned();
                context.emit(crate::events::Event::Image(saved_relative.clone()));
                context.stack.push(Value::String(saved_relative));
            }
            Ops::VideoQuality => {
                let quality = crate::video_gen::Quality::parse(&pop(context)?.expect_string()?)?;
                let folder = context.project.folder();
                let mut config = Config::load(&folder)?;
                config.video_quality = quality.name().into();
                config.save(&folder)?;
                context.say(&format!("Video quality: {}", quality.name()));
            }
            Ops::DraftVideo => {
                let prompt = pop(context)?.expect_string()?;
                let relative = pop(context)?.expect_string()?;
                let saved = draft_video(context, &relative, &prompt)?;
                context.stack.push(Value::String(saved));
            }
            Ops::FinishVideo => {
                let relative = pop(context)?.expect_string()?;
                let input = context.project.resolve(&relative)?;
                if !input.is_file() {
                    return Err(format!("`{}` is not a video in the project", relative.trim()));
                }
                let stem = input.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                let output = input.with_file_name(format!("{stem}-finished.mp4"));
                crate::video_gen::finish_video(&input, &output, &mut |line| context.progress(line))?;
                let saved = output
                    .strip_prefix(&context.project.root)
                    .map_err(|_| "video path is outside the project".to_string())?
                    .to_string_lossy()
                    .into_owned();
                context.emit(crate::events::Event::Video(saved.clone()));
                context.stack.push(Value::String(saved));
            }
            Ops::VideoSeconds => {
                let seconds = pop(context)?.expect_int()?;
                let seconds = u32::try_from(seconds).ok().filter(|&s| s <= 600).ok_or("video-seconds takes 0 to 600")?;
                let folder = context.project.folder();
                let mut config = Config::load(&folder)?;
                config.video_seconds = seconds;
                config.save(&folder)?;
                context.say(&if seconds == 0 { "Video length: default".to_string() } else { format!("Video length: {seconds} s") });
            }
            Ops::MakeVideo => {
                let prompt = pop(context)?.expect_string()?;
                let images = pop(context)?.expect_string()?;
                let relative = pop(context)?.expect_string()?;
                let requested = std::path::Path::new(relative.trim());
                if !matches!(requested.components().next_back(), Some(std::path::Component::Normal(_))) {
                    return Err("video output path must include a filename".into());
                }
                let mp4_relative = requested.with_extension("mp4");
                let output = context.project.resolve(mp4_relative.to_str().ok_or("video output path is not valid UTF-8")?)?;
                let keyframes = keyframe_paths(context, &images)?;
                let config = Config::load(&context.project.folder())?;
                let model = crate::image_models::video_model(&config)?;
                let quality = crate::video_gen::Quality::parse(&config.video_quality)?;
                let seconds = Some(config.video_seconds).filter(|&s| s > 0);
                let request = crate::video_gen::VideoRequest { prompt: &prompt, keyframes: &keyframes, quality, seconds };
                with_gpu_room(context, &model.clone(), |context| {
                    crate::video_gen::make_video(&model, &request, &output, &mut |line| context.progress(line))
                })?;
                let saved = output
                    .strip_prefix(&context.project.root)
                    .map_err(|_| "video path is outside the project".to_string())?
                    .to_string_lossy()
                    .into_owned();
                context.emit(crate::events::Event::Video(saved.clone()));
                context.stack.push(Value::String(saved));
            }
            Ops::Serve => {
                let action = pop(context)?.expect_string()?;
                let root = context.project.root.clone();
                let port = Config::load(&context.project.folder())?.web_port;
                let web = context
                    .web
                    .as_mut()
                    .ok_or("serve works in a chat session; run `forgeflow` or `forgeflow serve`")?;
                let message = match action.trim().to_ascii_lowercase().as_str() {
                    "start" | "on" => web.start(port, &root)?,
                    "stop" | "off" => web.stop(),
                    other => return Err(format!("serve takes \"start\" or \"stop\", not \"{other}\"")),
                };
                // Shown but not recorded, so the QR code never reaches the model's context.
                context.emit(crate::events::Event::Line(crate::events::Kind::Command, message));
            }
            Ops::EditImage => {
                let instruction = pop(context)?.expect_string()?;
                // A list, or comma-separated paths as the web UI sends for several photos.
                let images = match pop(context)? {
                    Value::List(items) => items.iter().map(Value::expect_string).collect::<Result<Vec<_>, _>>()?,
                    other => other.expect_string()?.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect(),
                };
                let mut inputs = vec![];
                for image in &images {
                    let path = context.project.resolve(image)?;
                    if !path.is_file() {
                        return Err(format!("`{}` is not an image file in the project", image.trim()));
                    }
                    inputs.push(path);
                }
                let config = Config::load(&context.project.folder())?;
                let model = crate::image_models::edit_model(&config)?;
                let image = with_gpu_room(context, &model.clone(), |context| {
                    crate::image_gen::LocalImageBackend::new(model)?.edit(&instruction, &inputs, &mut |line| context.progress(line))
                })?;
                let saved_path = crate::image_gen::save_image(&edit_output_path(&inputs[0]), &image)?;
                let saved_relative = saved_path
                    .strip_prefix(&context.project.root)
                    .map_err(|_| "edited image path is outside the project".to_string())?
                    .to_string_lossy()
                    .into_owned();
                context.emit(crate::events::Event::Image(saved_relative.clone()));
                context.stack.push(Value::String(saved_relative));
            }
            Ops::ReadLine => {
                let answer = util::read_line()?;
                context.stack.push(Value::String(answer));
            }
            Ops::AskLlm => {
                let question = pop(context)?.expect_string()?;
                let agent = Agent::new(context.provider);
                let answer = agent.ask(&question).map_err(|e| e.to_string())?;
                context.stack.push(Value::String(answer));
            }
            Ops::Print => {
                let value = pop(context)?;
                context.say(&value.to_text());
            }
            Ops::StringLiteral(s) => context.stack.push(Value::String(s.clone())),
            Ops::IntLiteral(i) => context.stack.push(Value::Int(*i)),
            Ops::BoolLiteral(b) => context.stack.push(Value::Bool(*b)),
            Ops::Register(r) => context.stack.push(Value::Register(*r)),
            Ops::SpecifyAgent(provider) => todo!(),
            Ops::LocalGet(name, _) => {
                let value = context.local_frames.iter().rev().find_map(|frame| frame.get(name)).cloned()
                    .ok_or_else(|| format!("no active input named `${name}`"))?;
                context.stack.push(value);
            }
            Ops::AddressOf(name) => context.stack.push(context.memory.address(name)),
            Ops::UserCall { name, .. } => crate::user_words::execute(context, name)?,
            Ops::Add | Ops::Subtract | Ops::Multiply | Ops::Divide => {
                let b = pop(context)?.expect_int()?;
                let a = pop(context)?.expect_int()?;
                context.stack.push(Value::Int(arithmetic(self, a, b)?));
            }
            Ops::Dup => {
                let top = context.stack.last().cloned().ok_or("dup needs a value")?;
                context.stack.push(top);
            }
            Ops::Drop => {
                pop(context)?;
            }
            Ops::Swap => {
                let top = pop(context)?;
                let below = pop(context)?;
                context.stack.push(top);
                context.stack.push(below);
            }
            Ops::Fetch => {
                let value = pop(context)?;
                let value = match value {
                    Value::Register(register) => context.registers.get(register)?.clone(),
                    Value::Address(name) => context.memory.get(&name)?.clone(),
                    other => return Err(format!("@ needs a register or address, got {}", other.describe())),
                };
                context.stack.push(value);
            }
            Ops::Store => {
                let target = pop(context)?;
                let value = pop(context)?;
                let label = context.store_label.clone();
                match target {
                    Value::Register(register) => context.registers.set(register, value, &label)?,
                    Value::Address(name) => context.memory.set(&name, value)?,
                    other => return Err(format!("! needs a register or address, got {}", other.describe())),
                }
            }
            Ops::ReadFile => {
                let relative = pop(context)?.expect_string()?;
                let path = context.project.resolve(&relative)?;
                let size = std::fs::metadata(&path)
                    .map_err(|e| format!("{relative}: {e}"))?
                    .len();
                if size > READ_MAX_BYTES {
                    return Err(format!("{relative} is {size} bytes; the limit is {READ_MAX_BYTES}"));
                }
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{relative}: {e}"))?;
                context.stack.push(Value::String(text));
            }
            Ops::Summarize => {
                let relative = pop(context)?.expect_string()?;
                let summary = crate::summary::for_file(context, &relative)?;
                context.stack.push(Value::String(crate::summary::to_text(&summary)));
            }
            Ops::ListDirectory => {
                let relative = pop(context)?.expect_string()?;
                let path = context.project.resolve(&relative)?;
                let entries = list_directory(&path).map_err(|e| format!("{relative}: {e}"))?;
                context
                    .stack
                    .push(Value::List(entries.into_iter().map(Value::String).collect()));
            }
            Ops::ListFiles => {
                let relative = pop(context)?.expect_string()?;
                let path = context.project.resolve(&relative)?;
                let entries = list_directory(&path).map_err(|e| format!("{relative}: {e}"))?;
                let files = entries.into_iter().filter(|e| !e.ends_with('/')).map(Value::String);
                context.stack.push(Value::List(files.collect()));
            }
            Ops::Each(body) => {
                let items = match pop(context)? {
                    Value::List(items) => items,
                    other => return Err(format!("`each` needs a list, got {}", other.describe())),
                };
                for item in items {
                    let depth = context.stack.len();
                    let label = item.to_text();
                    context.stack.push(item);
                    // One item failing does not stop the others.
                    if let Err(error) = crate::workflow::run_steps(context, body) {
                        context.stack.truncate(depth);
                        context.say(&format!("error ({label}): {error}"));
                    }
                }
            }
            Ops::RegView => {
                let end = pop(context)?.expect_int()?;
                let start = pop(context)?.expect_int()?;
                let register = pop(context)?.expect_register()?;
                let text = context.registers.get(register)?.to_text();
                context.stack.push(Value::String(view_lines(&text, start, end)));
            }
            Ops::RegSearch => {
                let pattern = pop(context)?.expect_string()?;
                let register = pop(context)?.expect_register()?;
                let text = context.registers.get(register)?.to_text();
                context.stack.push(Value::String(search_lines(&text, &pattern)));
            }
            Ops::RegEdit => {
                let replace = pop(context)?.expect_string()?;
                let find = pop(context)?.expect_string()?;
                let register = pop(context)?.expect_register()?;
                let text = context.registers.get(register)?.to_text();
                context
                    .stack
                    .push(Value::String(replace_once(&text, &find, &replace, register)?));
            }
            Ops::Emit => {
                let register = pop(context)?.expect_register()?;
                let text = context.registers.get(register)?.to_text();
                context.say(&text);
            }
            Ops::ShowRegisters => {
                let table = context.registers.table();
                context.say(&table);
            }
            Ops::Models => {
                let config = Config::load(&context.project.folder())?;
                let table = models::render_table(&Device::probe(), config.ctx, config.model.as_deref());
                context.say(&table);
            }
            Ops::Grep => {
                let pattern = pop(context)?.expect_string()?;
                let text = pop(context)?.expect_string()?;
                context.stack.push(Value::String(search_lines(&text, &pattern)));
            }
            Ops::Help => {
                let text = crate::translate::help_text();
                context.say(&text);
            }
            Ops::Answer => {
                let question = pop(context)?.expect_string()?;
                let text = pop(context)?.expect_string()?;
                answer(context, &text, &question)?;
            }
            Ops::Pull => {
                let id = pop(context)?.expect_string()?;
                models::pull(&id, &mut std::io::stderr())?;
            }
            Ops::UseModel => {
                let id = pop(context)?.expect_string()?;
                let (path, catalogued) = models::resolve_installed(&id)?;
                // Chat works with any model (a grammar shapes its output); only `agent` needs tool calls.
                match &catalogued {
                    Some(model) if !model.tool_calls => {
                        context.progress(format!("{id} has no native tool calling: chat works, but the `agent` word will not."))
                    }
                    None => context.progress(format!("{id} is not catalogued; the `agent` word may not work with it.")),
                    _ => {}
                }
                let folder = context.project.folder();
                let mut config = Config::load(&folder)?;
                config.model = Some(id.clone());
                config.save(&folder)?;
                // A running server holds the previous model; the next `agent` restarts it.
                context.server = None;
                context.say(&format!("Selected {id} ({})", path.display()));
            }
            Ops::ImageModels => {
                let config = Config::load(&context.project.folder())?;
                let table = crate::image_models::render_table(&config);
                context.say(&table);
            }
            Ops::UseImage => {
                let choice = pop(context)?.expect_string()?;
                let selection = crate::image_models::select(&context.project.folder(), &choice)?;
                context.say(&format!("Images: {selection}"));
            }
            Ops::Agent => {
                let task = pop(context)?.expect_string()?;
                let answer = agent_loop::run(context, &task)?;
                context.stack.push(Value::String(answer));
            }
        }
        std::io::stdout().flush().ok();
        Ok(())
    }

    /// The things the given step requires on the stack, listed bottom to top.
    pub fn inputs(&self) -> Vec<Type> {
        use TypeKind::*;
        match self {
            Ops::WriteFile => vec![Type::str("path", "file path"), Type::str("contents", "contents")],
            Ops::AskLlm => vec![Type::str("question", "the question to ask")],
            Ops::Print => vec![Type::new("value", Any, "the value to print")],
            Ops::Dup | Ops::Drop => vec![Type::new("value", Any, "a value")],
            Ops::Swap => vec![Type::new("a", Any, "a value"), Type::new("b", Any, "a value")],
            Ops::Fetch => vec![Type::new("address", Reference, "a register or address")],
            Ops::Emit => vec![Type::new("register", Register, "a register such as R5")],
            Ops::Store => vec![
                Type::new("value", Any, "the value, or $R5 to copy a register"),
                Type::new("register", Reference, "the register or address to store into"),
            ],
            Ops::ReadFile | Ops::ListDirectory | Ops::ListFiles | Ops::Summarize => {
                vec![Type::str("path", "path relative to the project root")]
            }
            Ops::MakeImage => vec![
                Type::str("path", "requested output path relative to the project root; extension is adjusted to match the generated format"),
                Type::str("prompt", "description of the image to generate"),
            ],
            Ops::Serve => vec![Type::str("action", "\"start\" or \"stop\"")],
            Ops::MakeVideo => vec![
                Type::str("path", "output path relative to the project root; saved as .mp4"),
                Type::str("images", "keyframe image path, comma-separated paths, a folder of images (used in name order), or \"\" for none"),
                Type::str("prompt", "what happens in the video"),
            ],
            Ops::VideoQuality => vec![Type::str("quality", "\"draft\" or \"full\"")],
            Ops::VideoSeconds => vec![Type::new("seconds", Int, "length in seconds, or 0 for the default")],
            Ops::FinishVideo => vec![Type::str("video", "path of the video, relative to the project root")],
            Ops::DraftVideo => vec![
                Type::str("path", "output path relative to the project root; saved as .mp4"),
                Type::str("prompt", "the shot: who is in it, what happens, the setting"),
            ],
            Ops::EditImage => vec![
                Type::new("image", Any, "path of the image to edit, relative to the project root, or a list of up to 4 paths whose first is edited and the rest are references"),
                Type::str("instruction", "what to change, such as \"make it night and add rain\""),
            ],
            Ops::Each(_) => vec![Type::new("items", List, "the list to go through")],
            Ops::RegView => vec![
                Type::new("register", Register, "a register such as R5"),
                Type::new("start", Int, "first line, from 1"),
                Type::new("end", Int, "last line, inclusive"),
            ],
            Ops::RegSearch => vec![
                Type::new("register", Register, "a register such as R5"),
                Type::str("pattern", "text to look for"),
            ],
            Ops::Grep => vec![Type::str("text", "text to search"), Type::str("pattern", "text to look for")],
            Ops::Add | Ops::Subtract | Ops::Multiply | Ops::Divide => {
                vec![Type::new("a", Int, "an integer"), Type::new("b", Int, "an integer")]
            }
            Ops::Answer => vec![
                Type::str("context", "text the reply is about, or \"\""),
                Type::str("question", "what to reply to"),
            ],
            Ops::RegEdit => vec![
                Type::new("register", Register, "the register holding the text"),
                Type::str("find", "text that occurs exactly once"),
                Type::str("replace", "replacement text"),
            ],
            Ops::Pull | Ops::UseModel => vec![Type::str("id", "a model ID from `models`")],
            Ops::UseImage => vec![Type::str("id", "`openrouter`, an OpenRouter model ID, or an ID from `image_models`")],
            Ops::Agent => vec![Type::str("task", "what the agent should do")],
            Ops::ImageModels
            | Ops::ReadLine
            | Ops::StringLiteral(_)
            | Ops::IntLiteral(_)
            | Ops::BoolLiteral(_)
            | Ops::Register(_)
            | Ops::SpecifyAgent(_)
            | Ops::AllCliArgs
            | Ops::ShowRegisters
            | Ops::Help
            | Ops::Models => vec![],
            Ops::LocalGet(_, _) => vec![],
            Ops::AddressOf(_) => vec![],
            Ops::UserCall { inputs, .. } => inputs.iter().map(|kind| Type::new("input", *kind, "user word input")).collect(),
        }
    }

    /// The things the given step puts on the stack.
    pub fn outputs(&self) -> Vec<Type> {
        use TypeKind::*;
        match self {
            Ops::AllCliArgs => vec![Type::str("args", "the args as a string")],
            Ops::ReadLine => vec![Type::str("line", "A line the user wrote")],
            Ops::AskLlm => vec![Type::str("response", "The response")],
            Ops::StringLiteral(_) => vec![Type::str("literal", "The string literal")],
            Ops::IntLiteral(_) => vec![Type::new("literal", Int, "The integer literal")],
            Ops::BoolLiteral(_) => vec![Type::new("literal", Bool, "The boolean literal")],
            Ops::Register(_) => vec![Type::new("register", Register, "The register")],
            Ops::Dup => vec![Type::new("value", Any, "a value"), Type::new("copy", Any, "a value")],
            Ops::Swap => vec![Type::new("b", Any, "a value"), Type::new("a", Any, "a value")],
            Ops::Fetch => vec![Type::new("value", Any, "the register's value")],
            Ops::ReadFile => vec![Type::str("text", "the file's contents")],
            Ops::Summarize => vec![Type::str("summary", "the file's summary")],
            Ops::MakeImage => vec![Type::str("saved_path", "project-relative path of the generated image")],
            Ops::EditImage => vec![Type::str("saved_path", "project-relative path of the edited image")],
            Ops::MakeVideo => vec![Type::str("saved_path", "project-relative path of the video")],
            Ops::FinishVideo => vec![Type::str("saved_path", "project-relative path of the finished video")],
            Ops::DraftVideo => vec![Type::str("saved_path", "project-relative path of the draft video")],
            Ops::ListDirectory => vec![Type::new("entries", List, "the folder's entries")],
            Ops::ListFiles => vec![Type::new("files", List, "the folder's files")],
            Ops::RegView | Ops::RegSearch | Ops::Grep => vec![Type::str("lines", "numbered lines")],
            Ops::Add | Ops::Subtract | Ops::Multiply | Ops::Divide => {
                vec![Type::new("result", Int, "the result")]
            }
            Ops::RegEdit => vec![Type::str("text", "the edited text")],
            Ops::Agent => vec![Type::str("answer", "the agent's final answer")],
            Ops::SpecifyAgent(_)
            | Ops::WriteFile
            | Ops::Print
            | Ops::Drop
            | Ops::Store
            | Ops::Emit
            | Ops::Each(_)
            | Ops::Answer
            | Ops::Help
            | Ops::ShowRegisters
            | Ops::Models
            | Ops::Pull
            | Ops::UseModel
            | Ops::ImageModels
            | Ops::UseImage
            | Ops::Serve
            | Ops::VideoQuality
            | Ops::VideoSeconds => vec![],
            Ops::LocalGet(_, kind) => vec![Type::new("local", *kind, "named input")],
            Ops::AddressOf(_) => vec![Type::new("address", Address, "durable memory address")],
            Ops::UserCall { outputs, .. } => outputs.iter().map(|kind| Type::new("output", *kind, "user word output")).collect(),
        }
    }
}

/// Checked integer arithmetic: overflow and division by zero are errors, not wraparound.
fn arithmetic(op: &Ops, a: i64, b: i64) -> Result<i64, String> {
    let result = match op {
        Ops::Add => a.checked_add(b),
        Ops::Subtract => a.checked_sub(b),
        Ops::Multiply => a.checked_mul(b),
        Ops::Divide if b == 0 => return Err("division by zero".into()),
        Ops::Divide => a.checked_div(b),
        _ => unreachable!("arithmetic called with {}", op.name()),
    };
    result.ok_or_else(|| format!("{a} {} {b} overflows", op.name()))
}

/// Streams a reply from the local model about `text`, trimming text the context window cannot hold.
fn answer(context: &mut Context, text: &str, question: &str) -> Result<(), String> {
    let (backend, config) = chat::connect(context)?;
    // Leave room for the reply. Dense text such as lockfile hashes runs near two
    // characters per token, so this bound holds for it too.
    let max_chars = (config.ctx.saturating_sub(ANSWER_RESERVED_TOKENS) * ANSWER_CHARS_PER_TOKEN) as usize;
    let trimmed = text.chars().count() > max_chars;
    let mut text: String = text.chars().take(max_chars).collect();
    if trimmed {
        text.push_str("\n[trimmed]");
    }
    let prompt = if text.trim().is_empty() {
        question.to_string()
    } else {
        format!("{question}\n\n---\n{text}")
    };
    let messages = [
        serde_json::json!({"role": "system", "content": "Reply concisely and directly. Base your reply on the provided text when there is one. When asked how to do something, include a concrete, copyable example using valid ForgeFlow syntax, and briefly explain it. Preserve the exact syntax in the provided text; ForgeFlow stores with `value register !` and fetches with `register @`."}),
        serde_json::json!({"role": "user", "content": prompt}),
    ];
    let options = StepOptions {
        temperature: Some(0.0),
        no_thinking: true,
        slot: Some(crate::server::ANSWER_SLOT),
        ..StepOptions::default()
    };
    let sink = &mut context.sink;
    let turn = backend.step(&messages, &[], &options, &mut |delta| {
        if let Delta::Text(t) = delta {
            sink.emit(crate::events::Event::Text(crate::events::Kind::Chat, t));
        }
    })?;
    context.emit(crate::events::Event::Text(crate::events::Kind::Chat, "\n".into()));
    context.printed.push_str(&turn.content);
    context.printed.push('\n');
    Ok(())
}

fn pop(context: &mut Context) -> Result<Value, String> {
    context.stack.pop().ok_or_else(|| "the stack is empty".to_string())
}

/// Folders first, then files, each alphabetical; folders end with `/`.
fn list_directory(path: &std::path::Path) -> std::io::Result<Vec<String>> {
    let mut folders = vec![];
    let mut files = vec![];
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() {
            folders.push(format!("{name}/"));
        } else {
            files.push(name);
        }
    }
    folders.sort();
    files.sort();
    folders.extend(files);
    Ok(folders)
}

/// Numbered lines `start..=end` (1-based); 0 means "from the start" or "as far as allowed".
pub fn view_lines(text: &str, start: i64, end: i64) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = start.max(1) as usize;
    let limit = start + VIEW_MAX_LINES - 1;
    let end = if end <= 0 { limit } else { (end as usize).min(limit) }.min(lines.len());
    if start > lines.len() {
        return format!(
            "(start {start} is past the end; the text has {} lines, so use a start from 1 to {})",
            lines.len(),
            lines.len()
        );
    }
    bounded_numbered_lines((start..=end).map(|n| (n, lines[n - 1])), lines.len() - end)
}

/// Numbered lines containing `pattern`, ignoring case; capped like `reg_view`.
pub fn search_lines(text: &str, pattern: &str) -> String {
    let needle = pattern.to_lowercase();
    let matches: Vec<(usize, &str)> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(&needle))
        .map(|(i, line)| (i + 1, line))
        .collect();
    if matches.is_empty() {
        return format!("(no lines contain {pattern:?})");
    }
    let total = matches.len();
    bounded_numbered_lines(matches.into_iter().take(VIEW_MAX_LINES), total.saturating_sub(VIEW_MAX_LINES))
}

fn bounded_numbered_lines<'a>(lines: impl Iterator<Item = (usize, &'a str)>, more: usize) -> String {
    let mut out = String::new();
    let mut omitted = more;
    let mut byte_capped = false;
    for (number, line) in lines {
        let row = format!("{number}\t{line}");
        let needed = row.len() + usize::from(!out.is_empty());
        if out.len() + needed > VIEW_MAX_BYTES {
            omitted += 1;
            byte_capped = true;
            continue;
        }
        if !out.is_empty() { out.push('\n'); }
        out.push_str(&row);
    }
    if omitted > 0 {
        let suffix = if byte_capped {
            format!("\n({omitted} more lines or matches; output capped at {VIEW_MAX_BYTES} bytes)")
        } else {
            format!("\n({omitted} more lines)")
        };
        if out.len() + suffix.len() <= VIEW_MAX_BYTES { out.push_str(&suffix); }
    }
    out
}

fn replace_once(text: &str, find: &str, replace: &str, register: usize) -> Result<String, String> {
    if find.is_empty() {
        return Err("`find` must not be empty".into());
    }
    match text.matches(find).count() {
        1 => Ok(text.replacen(find, replace, 1)),
        0 => Err(format!("`find` does not occur in R{register}")),
        n => Err(format!(
            "`find` occurs {n} times in R{register}; include more surrounding text"
        )),
    }
}

/// Runs `run` with room on the GPU for `model`: when there is too little free memory, this
/// session's llama-server is stopped first and started again afterwards, so callers such as
/// the agent loop keep a working model. A server another session owns is left alone.
fn with_gpu_room<T>(
    context: &mut Context,
    model: &crate::image_models::ImageModel,
    run: impl FnOnce(&mut Context) -> Result<T, String>,
) -> Result<T, String> {
    let paused = pause_for(context, model);
    let result = run(context);
    if paused {
        context.progress("Resuming llama-server…");
        if let Err(error) = chat::connect(context) {
            context.progress(format!("warning: could not restart llama-server ({error}); the next message will try again"));
        }
    }
    result
}

/// Stops this session's llama-server if the GPU lacks room for `model`; true if it did.
fn pause_for(context: &mut Context, model: &crate::image_models::ImageModel) -> bool {
    let owned = context.server.as_ref().is_some_and(|s| s.is_owned());
    if !owned {
        return false;
    }
    let needed = crate::image_gen::gpu_bytes_needed(model);
    let Some(free) = crate::device::gpu_free_bytes() else { return false };
    if free >= needed {
        return false;
    }
    context.progress(format!(
        "Pausing llama-server to free GPU memory for {} ({} free, ~{} needed)…",
        model.id,
        models::gb(free),
        models::gb(needed)
    ));
    context.server = None;
    true
}

/// `draft-video`: plans keyframes with the chat model (or plain wording without one),
/// generates the first and edits it into the rest, then animates a draft through them.
/// The keyframes are saved beside the video for checking, editing and reuse.
fn draft_video(context: &mut Context, relative: &str, prompt: &str) -> Result<String, String> {
    use crate::events::Event;
    let requested = std::path::Path::new(relative.trim());
    if !matches!(requested.components().next_back(), Some(std::path::Component::Normal(_))) {
        return Err("video output path must include a filename".into());
    }
    let output = context.project.resolve(requested.with_extension("mp4").to_str().ok_or("video output path is not valid UTF-8")?)?;
    let config = Config::load(&context.project.folder())?;
    let video_model = crate::image_models::video_model(&config)?;
    let edit_model = crate::image_models::edit_model(&config)?;
    let (image_config, note) = crate::image_models::effective_generation(&config);
    if let Some(note) = note {
        context.progress(note);
    }
    let seconds = Some(config.video_seconds).filter(|&s| s > 0);
    let count = crate::video_gen::keyframe_count(config.video_seconds, video_model.fps);

    // Plan while the chat model is loaded; image and video work then pause it once.
    context.progress(format!("Planning {count} keyframes…"));
    let plan = plan_keyframes(context, prompt, count);
    context.say(&format!("Opening frame: {}", plan.start));
    for (index, change) in plan.changes.iter().enumerate() {
        context.say(&format!("Keyframe {}: {change}", index + 2));
    }

    let stem = output.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let key_path = |n: usize| output.with_file_name(format!("{stem}-key-{n}.png"));
    let root = context.project.root.clone();
    let relative_to_root = |path: &std::path::Path| path.strip_prefix(&root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    with_gpu_room(context, &video_model.clone(), |context| {
        let first = key_path(1);
        let image = crate::image_gen::backend(&image_config)?.generate(&plan.start, &mut |line| context.progress(line))?;
        let first = crate::image_gen::save_image(&first, &image)?;
        context.emit(Event::Image(relative_to_root(&first)));
        let mut keyframes = vec![first.clone()];
        let editor = crate::image_gen::LocalImageBackend::new(edit_model.clone())?;
        for (index, change) in plan.changes.iter().enumerate() {
            // Each keyframe edits the first, so all of them share its characters and setting.
            let image = editor.edit(change, std::slice::from_ref(&first), &mut |line| context.progress(line))?;
            let path = crate::image_gen::save_image(&key_path(index + 2), &image)?;
            context.emit(Event::Image(relative_to_root(&path)));
            keyframes.push(path);
        }
        let request = crate::video_gen::VideoRequest {
            prompt: &plan.motion,
            keyframes: &keyframes,
            quality: crate::video_gen::Quality::Draft,
            seconds,
        };
        crate::video_gen::make_video(&video_model, &request, &output, &mut |line| context.progress(line))
    })?;
    let saved = relative_to_root(&output);
    context.emit(Event::Video(saved.clone()));
    Ok(saved)
}

/// Asks the chat model for a keyframe plan, falling back to plain wording if there is no
/// model or its reply cannot be used.
fn plan_keyframes(context: &mut Context, prompt: &str, count: usize) -> crate::video_gen::KeyframePlan {
    let planned = chat::connect(context).ok().and_then(|(backend, _)| {
        let options = StepOptions { temperature: Some(0.4), max_tokens: Some(900), no_thinking: true, slot: Some(crate::server::ANSWER_SLOT), ..StepOptions::default() };
        let turn = backend.step(&crate::video_gen::plan_messages(prompt, count), &[], &options, &mut |_| {}).ok()?;
        crate::video_gen::parse_plan(&turn.content, count)
    });
    planned.unwrap_or_else(|| {
        context.progress("No usable plan from the chat model; using the prompt as written.");
        crate::video_gen::fallback_plan(prompt, count)
    })
}

/// Keyframes named by `images`: nothing for "", a folder's images in name order, or
/// comma-separated project paths.
fn keyframe_paths(context: &Context, images: &str) -> Result<Vec<std::path::PathBuf>, String> {
    let is_image = |path: &std::path::Path| {
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["png", "jpg", "jpeg", "webp"].contains(&e.to_ascii_lowercase().as_str()))
    };
    let mut keyframes = vec![];
    for part in images.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let path = context.project.resolve(part)?;
        if path.is_dir() {
            let mut found: Vec<_> = std::fs::read_dir(&path)
                .map_err(|e| format!("{part}: {e}"))?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|p| p.is_file() && is_image(p))
                .collect();
            found.sort();
            if found.is_empty() {
                return Err(format!("`{part}` has no images"));
            }
            keyframes.extend(found);
        } else if path.is_file() {
            keyframes.push(path);
        } else {
            return Err(format!("`{part}` is not an image or folder in the project"));
        }
    }
    Ok(keyframes)
}

/// `NAME-edit-N.png` beside `input`, with the first N not already taken.
fn edit_output_path(input: &std::path::Path) -> std::path::PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    // Editing an edit continues its numbering rather than nesting `-edit-1-edit-1`.
    let base = match stem.rsplit_once("-edit-") {
        Some((base, n)) if n.parse::<u32>().is_ok() => base.to_string(),
        _ => stem.into_owned(),
    };
    let dir = input.parent().unwrap_or(std::path::Path::new("."));
    (1..)
        .map(|n| dir.join(format!("{base}-edit-{n}.png")))
        .find(|path| !path.exists())
        .expect("some edit number is free")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_are_numbered_beside_the_original_without_nesting() {
        let dir = std::env::temp_dir().join(format!("forgeflow-edit-names-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("arena.jpg");
        assert_eq!(edit_output_path(&input), dir.join("arena-edit-1.png"));
        std::fs::write(dir.join("arena-edit-1.png"), b"").unwrap();
        assert_eq!(edit_output_path(&input), dir.join("arena-edit-2.png"));
        assert_eq!(edit_output_path(&dir.join("arena-edit-1.png")), dir.join("arena-edit-2.png"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn views_numbered_line_ranges() {
        let text = "a\nb\nc\nd";
        let cases = [
            (1, 2, "1\ta\n2\tb\n(2 more lines)"),
            (3, 0, "3\tc\n4\td"),
            (0, 1, "1\ta\n(3 more lines)"),
            (9, 10, "(start 9 is past the end; the text has 4 lines, so use a start from 1 to 4)"),
        ];
        for (start, end, expected) in cases {
            assert_eq!(view_lines(text, start, end), expected, "{start}-{end}");
        }
    }

    #[test]
    fn view_caps_the_line_count() {
        let text = (1..=500).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let view = view_lines(&text, 1, 0);
        assert_eq!(view.lines().count(), VIEW_MAX_LINES + 1);
        assert!(view.ends_with("(300 more lines)"));
    }

    #[test]
    fn searches_lines_ignoring_case() {
        let text = "fn a() {}\nlet M4 = 1;\nlet m4_pro = 2;";
        assert_eq!(search_lines(text, "m4"), "2\tlet M4 = 1;\n3\tlet m4_pro = 2;");
        assert_eq!(search_lines(text, "zzz"), "(no lines contain \"zzz\")");
    }

    #[test]
    fn arithmetic_is_checked() {
        let cases = [
            (Ops::Add, 1, 1, Ok(2)),
            (Ops::Subtract, 2, 5, Ok(-3)),
            (Ops::Multiply, 12, 7, Ok(84)),
            (Ops::Divide, 7, 2, Ok(3)),
            (Ops::Divide, 1, 0, Err("division by zero".to_string())),
            (Ops::Add, i64::MAX, 1, Err(format!("{} + 1 overflows", i64::MAX))),
        ];
        for (op, a, b, expected) in cases {
            assert_eq!(arithmetic(&op, a, b), expected, "{a} {} {b}", op.name());
        }
    }

    #[test]
    fn replace_requires_exactly_one_match() {
        assert_eq!(replace_once("a b c", "b", "x", 5), Ok("a x c".into()));
        assert!(replace_once("a b b", "b", "x", 5).unwrap_err().contains("2 times"));
        assert!(replace_once("a b c", "z", "x", 5).unwrap_err().contains("does not occur"));
        assert!(replace_once("a", "", "x", 5).is_err());
    }
}
