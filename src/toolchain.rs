//! Finds llama.cpp's `llama-server` and stable-diffusion.cpp's `sd-cli`, cloning and
//! building them under the models directory with the best GPU backend this machine
//! supports when they are missing.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::device::{Device, Gpu};
use crate::models::models_dir;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tool {
    LlamaServer,
    SdCli,
}

impl Tool {
    pub const ALL: [Tool; 2] = [Tool::LlamaServer, Tool::SdCli];

    pub fn binary_name(self) -> &'static str {
        match self {
            Tool::LlamaServer => "llama-server",
            Tool::SdCli => "sd-cli",
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            Tool::LlamaServer => "FORGEFLOW_LLAMA_SERVER",
            Tool::SdCli => "FORGEFLOW_SD_CLI",
        }
    }

    fn repo(self) -> &'static str {
        match self {
            Tool::LlamaServer => "https://github.com/ggml-org/llama.cpp",
            Tool::SdCli => "https://github.com/leejet/stable-diffusion.cpp",
        }
    }

    /// The `owner/name` GitHub path, for the release check.
    fn github(self) -> &'static str {
        match self {
            Tool::LlamaServer => "ggml-org/llama.cpp",
            Tool::SdCli => "leejet/stable-diffusion.cpp",
        }
    }

    /// The release tag ForgeFlow builds. Bump it after checking the new release still
    /// builds and runs; startup warns when a newer release is out.
    pub fn pinned(self) -> &'static str {
        match self {
            Tool::LlamaServer => "v0.9.0",
            Tool::SdCli => "master-1086-465adf3",
        }
    }

    pub fn checkout_name(self) -> &'static str {
        match self {
            Tool::LlamaServer => "llama.cpp",
            Tool::SdCli => "stable-diffusion.cpp",
        }
    }

    pub fn source_dir(self) -> PathBuf {
        models_dir().join(self.checkout_name())
    }

    fn built_path(self) -> PathBuf {
        self.source_dir().join("build/bin").join(self.binary_name())
    }

    /// The CMake option enabling `backend` in this project; `None` builds for the CPU.
    fn backend_flag(self, backend: Backend) -> Option<&'static str> {
        match (self, backend) {
            (_, Backend::Cpu) => None,
            (Tool::LlamaServer, Backend::Metal) => Some("-DGGML_METAL=ON"),
            (Tool::LlamaServer, Backend::Cuda) => Some("-DGGML_CUDA=ON"),
            (Tool::LlamaServer, Backend::Vulkan) => Some("-DGGML_VULKAN=ON"),
            (Tool::SdCli, Backend::Metal) => Some("-DSD_METAL=ON"),
            (Tool::SdCli, Backend::Cuda) => Some("-DSD_CUDA=ON"),
            (Tool::SdCli, Backend::Vulkan) => Some("-DSD_VULKAN=ON"),
        }
    }

    /// Project options beyond the backend: llama.cpp builds only the server, without
    /// libcurl since ForgeFlow downloads models itself.
    fn extra_flags(self) -> &'static [&'static str] {
        match self {
            Tool::LlamaServer => &["-DLLAMA_CURL=OFF", "-DLLAMA_BUILD_TESTS=OFF", "-DLLAMA_BUILD_EXAMPLES=OFF"],
            Tool::SdCli => &[],
        }
    }

    /// The CMake target to build; `None` builds everything.
    fn target(self) -> Option<&'static str> {
        match self {
            Tool::LlamaServer => Some("llama-server"),
            // The CLI target was renamed over time, so build all and check the binary.
            Tool::SdCli => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Backend {
    Metal,
    Cuda,
    Vulkan,
    Cpu,
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Backend::Metal => "metal",
            Backend::Cuda => "cuda",
            Backend::Vulkan => "vulkan",
            Backend::Cpu => "cpu",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [Backend::Metal, Backend::Cuda, Backend::Vulkan, Backend::Cpu]
            .into_iter()
            .find(|b| b.name().eq_ignore_ascii_case(name.trim()))
    }
}

/// The tool's path from its environment variable, a ForgeFlow build, or `PATH`.
pub fn find(tool: Tool) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(tool.env_var()) {
        return Some(path.into());
    }
    let built = tool.built_path();
    if built.is_file() {
        return Some(built);
    }
    which(tool.binary_name())
}

/// Finds `tool`, building it first when it is missing. Progress goes to stderr and the
/// compiler output to a log beside the checkout. An outdated build is still used; startup
/// warns about it and `forgeflow install` rebuilds it.
pub fn ensure(tool: Tool) -> Result<PathBuf, String> {
    if let Some(path) = find(tool) {
        return Ok(path);
    }
    build(tool)
}

/// How `tool` is installed.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// From its environment variable or `PATH`, so not ForgeFlow's to update.
    External(PathBuf),
    /// Built by ForgeFlow at the pinned release.
    Current(PathBuf),
    /// Built from another release, or before ForgeFlow recorded one.
    Outdated { path: PathBuf, built: Option<String> },
    Missing,
}

pub fn status(tool: Tool) -> Status {
    let built = tool.built_path();
    match find(tool) {
        None => Status::Missing,
        Some(path) if path != built => Status::External(path),
        Some(path) => match built_info(tool) {
            Some(info) if info.tag == tool.pinned() => Status::Current(path),
            info => Status::Outdated { path, built: info.map(|i| i.tag) },
        },
    }
}

/// Builds `tool` when it is missing or not at the pinned release.
pub fn install(tool: Tool) -> Result<Status, String> {
    match status(tool) {
        Status::Missing | Status::Outdated { .. } => {
            build(tool)?;
            Ok(status(tool))
        }
        current => Ok(current),
    }
}

/// The error for callers that cannot build in place, such as a full-screen editor.
pub fn missing(tool: Tool) -> String {
    format!(
        "{} not found. Run `forgeflow install` to build it, or set {}",
        tool.binary_name(),
        tool.env_var()
    )
}

/// What to build with on this machine, and why other backends were passed over.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub backend: Backend,
    /// Extra CMake arguments for the backend, such as the CUDA architecture.
    pub cmake_args: Vec<String>,
    pub notes: Vec<String>,
}

/// Chooses the fastest backend the machine can build: Metal on Apple Silicon, CUDA with
/// an NVIDIA card and toolkit, Vulkan with its SDK, otherwise CPU. `FORGEFLOW_BUILD_BACKEND`
/// (metal, cuda, vulkan, or cpu) overrides the choice.
pub fn plan(device: &Device) -> Result<Plan, String> {
    let forced = match std::env::var("FORGEFLOW_BUILD_BACKEND") {
        Ok(name) => Some(Backend::parse(&name).ok_or(format!(
            "FORGEFLOW_BUILD_BACKEND={name} is not one of metal, cuda, vulkan, cpu"
        ))?),
        Err(_) => None,
    };
    let mut notes = vec![];
    let nvcc = find_nvcc();
    let backend = match forced {
        Some(backend) => backend,
        None if cfg!(target_os = "macos") && device.apple_silicon => Backend::Metal,
        None if device.gpu.is_some() && nvcc.is_some() => Backend::Cuda,
        None if device.gpu.is_some() && has_vulkan() => {
            notes.push("CUDA toolkit (nvcc) not found; using Vulkan, which is slower on NVIDIA cards".into());
            Backend::Vulkan
        }
        None if device.gpu.is_some() => {
            return Err(format!(
                "found {} but no CUDA toolkit to build for it. {}\nOr set FORGEFLOW_BUILD_BACKEND=cpu to build without the GPU.",
                device.gpu.as_ref().map(|g| g.name.as_str()).unwrap_or("an NVIDIA GPU"),
                install_hint(&["nvcc"]),
            ));
        }
        None if has_vulkan() => Backend::Vulkan,
        None => Backend::Cpu,
    };
    let mut cmake_args = vec![];
    if backend == Backend::Cuda {
        let nvcc = nvcc.ok_or_else(|| format!("FORGEFLOW_BUILD_BACKEND=cuda but nvcc was not found. {}", install_hint(&["nvcc"])))?;
        cmake_args.push(format!("-DCMAKE_CUDA_COMPILER={}", nvcc.display()));
        // Compiling only for the installed cards cuts CUDA build time several-fold.
        if let Some(gpu) = &device.gpu {
            cmake_args.extend(cuda_architectures(gpu));
        }
        cmake_args.extend(cuda_host_compiler(&nvcc, &mut notes));
    }
    Ok(Plan { backend, cmake_args, notes })
}

fn cuda_architectures(gpu: &Gpu) -> Option<String> {
    (!gpu.compute_caps.is_empty()).then(|| format!("-DCMAKE_CUDA_ARCHITECTURES={}", gpu.compute_caps.join(";")))
}

/// Each CUDA release supports host GCC only up to some version; when the default `g++`
/// is newer, use an installed older one, or else let nvcc try the newer one anyway.
fn cuda_host_compiler(nvcc: &Path, notes: &mut Vec<String>) -> Vec<String> {
    if cfg!(target_os = "macos") {
        return vec![];
    }
    let Some(cuda) = command_output(nvcc, &["--version"]).and_then(|v| parse_cuda_version(&v)) else {
        return vec![];
    };
    let Some(gcc) = command_output(Path::new("g++"), &["-dumpversion"]).and_then(|v| v.trim().split('.').next()?.parse::<u32>().ok())
    else {
        return vec![];
    };
    let max = max_gcc_for_cuda(cuda);
    if gcc <= max {
        return vec![];
    }
    if let Some(older) = (9..=max).rev().find_map(|v| which(&format!("g++-{v}"))) {
        notes.push(format!(
            "CUDA {}.{} supports GCC up to {max}; compiling CUDA code with {}",
            cuda.0,
            cuda.1,
            older.display()
        ));
        return vec![format!("-DCMAKE_CUDA_HOST_COMPILER={}", older.display())];
    }
    notes.push(format!(
        "CUDA {}.{} supports GCC up to {max} but g++ is {gcc}; forcing it (install g++-{max} if the build fails)",
        cuda.0, cuda.1
    ));
    vec!["-DCMAKE_CUDA_FLAGS=-allow-unsupported-compiler".into()]
}

fn parse_cuda_version(nvcc_version: &str) -> Option<(u32, u32)> {
    let release = nvcc_version.split("release ").nth(1)?;
    let version = release.split(',').next()?;
    let (major, minor) = version.trim().split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The newest host GCC major version each CUDA release accepts.
fn max_gcc_for_cuda((major, minor): (u32, u32)) -> u32 {
    match (major, minor) {
        (..=10, _) => 8,
        (11, 0..=3) => 10,
        (11, _) => 11,
        (12, 0..=3) => 12,
        (12, 4..=8) => 13,
        _ => 14,
    }
}

fn find_nvcc() -> Option<PathBuf> {
    which("nvcc").or_else(|| {
        let default = PathBuf::from("/usr/local/cuda/bin/nvcc");
        default.is_file().then_some(default)
    })
}

/// Vulkan builds need the shader compiler and the loader's headers.
fn has_vulkan() -> bool {
    which("glslc").is_some()
        && ["/usr/include/vulkan/vulkan.h", "/usr/local/include/vulkan/vulkan.h"]
            .iter()
            .any(|p| Path::new(p).is_file())
}

fn build(tool: Tool) -> Result<PathBuf, String> {
    let device = Device::probe();
    let plan = plan(&device)?;
    let missing: Vec<&str> = ["git", "cmake", "c++"].into_iter().filter(|t| which(t).is_none()).collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} not found, and building it needs {}. {}",
            tool.binary_name(),
            missing.join(", "),
            install_hint(&missing)
        ));
    }
    let source = tool.source_dir();
    std::fs::create_dir_all(models_dir()).map_err(|e| format!("could not create {}: {e}", models_dir().display()))?;
    let log_path = models_dir().join(format!("build-{}.log", tool.checkout_name()));
    eprintln!(
        "{} not found; building it for {} (log: {})",
        tool.binary_name(),
        plan.backend.name(),
        log_path.display()
    );
    for note in &plan.notes {
        eprintln!("  note: {note}");
    }
    let log = File::create(&log_path).map_err(|e| format!("could not create {}: {e}", log_path.display()))?;

    let tag = tool.pinned();
    if source.join("CMakeLists.txt").is_file() {
        eprintln!("  switching {} to {tag}", source.display());
        let git = |args: &[&str]| {
            let mut command = Command::new("git");
            command.arg("-C").arg(&source).args(args);
            command
        };
        run(&log, &log_path, &mut git(&["fetch", "--depth", "1", "origin", "tag", tag]))?;
        run(&log, &log_path, &mut git(&["checkout", "--force", tag]))?;
        run(&log, &log_path, &mut git(&["submodule", "update", "--init", "--recursive", "--depth", "1"]))?;
    } else {
        if source.exists() {
            return Err(format!(
                "{} exists but is not a {} checkout; remove it and try again",
                source.display(),
                tool.checkout_name()
            ));
        }
        eprintln!("  cloning {} at {tag}", tool.repo());
        run(
            &log,
            &log_path,
            Command::new("git")
                .args(["clone", "--depth", "1", "--branch", tag, "--recursive", "--shallow-submodules", tool.repo()])
                .arg(&source),
        )?;
    }

    let build_dir = source.join("build");
    let mut configure = Command::new("cmake");
    configure.arg("-S").arg(&source).arg("-B").arg(&build_dir).arg("-DCMAKE_BUILD_TYPE=Release");
    if which("ninja").is_some() {
        configure.args(["-G", "Ninja"]);
    }
    configure.args(tool.backend_flag(plan.backend)).args(tool.extra_flags()).args(&plan.cmake_args);
    eprintln!("  configuring");
    run(&log, &log_path, &mut configure)?;

    let jobs = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut compile = Command::new("cmake");
    compile.arg("--build").arg(&build_dir).args(["--config", "Release", "--parallel", &jobs.to_string()]);
    if let Some(target) = tool.target() {
        compile.args(["--target", target]);
    }
    eprintln!(
        "  compiling with {jobs} jobs{}",
        if plan.backend == Backend::Cuda { " (CUDA kernels take several minutes)" } else { "" }
    );
    run(&log, &log_path, &mut compile)?;

    let built = tool.built_path();
    if !built.is_file() {
        return Err(format!("the build finished but {} is missing; see {}", built.display(), log_path.display()));
    }
    let _ = std::fs::write(build_dir.join(BUILD_INFO), format!("{}\n{tag}\n", plan.backend.name()));
    eprintln!("  built {}", built.display());
    Ok(built)
}

/// Runs `command` with its output appended to the build log, failing with the log's tail.
fn run(log: &File, log_path: &Path, command: &mut Command) -> Result<(), String> {
    let out = log.try_clone().map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .status()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if status.success() {
        return Ok(());
    }
    let text = std::fs::read_to_string(log_path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(20)..].join("\n");
    Err(format!("{program} failed ({status}); last lines of {}:\n{tail}", log_path.display()))
}

/// Written beside each build: the backend on the first line, the release tag on the second.
const BUILD_INFO: &str = "forgeflow-build";

#[derive(Debug, Clone, PartialEq)]
pub struct BuildInfo {
    pub backend: String,
    pub tag: String,
}

/// What ForgeFlow built `tool` with, when it built it.
pub fn built_info(tool: Tool) -> Option<BuildInfo> {
    let text = std::fs::read_to_string(tool.source_dir().join("build").join(BUILD_INFO)).ok()?;
    let mut lines = text.lines().map(str::trim);
    Some(BuildInfo { backend: lines.next()?.to_string(), tag: lines.next()?.to_string() })
}

/// How often startup asks GitHub for new releases.
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Warnings for startup: builds older than the pins, and releases newer than the pins.
/// The release check runs at most daily with a short timeout, and any failure (offline,
/// rate limited, unexpected response) just skips it. `FORGEFLOW_NO_UPDATE_CHECK` turns it off.
pub fn startup_warnings() -> Vec<String> {
    let mut warnings = vec![];
    for tool in Tool::ALL {
        if let Status::Outdated { built, .. } = status(tool) {
            warnings.push(format!(
                "{} was built from {} but ForgeFlow pins {}; run `forgeflow install` to rebuild",
                tool.checkout_name(),
                built.as_deref().unwrap_or("an unknown release"),
                tool.pinned()
            ));
        }
    }
    if std::env::var_os("FORGEFLOW_NO_UPDATE_CHECK").is_none() {
        for (tool, latest) in latest_releases() {
            if latest != tool.pinned() {
                warnings.push(format!(
                    "{} {latest} is out (ForgeFlow pins {}); update `pinned` in src/toolchain.rs once it is tested",
                    tool.checkout_name(),
                    tool.pinned()
                ));
            }
        }
    }
    warnings
}

/// The latest release tag of each tool, from a cache younger than a day or else GitHub.
fn latest_releases() -> Vec<(Tool, String)> {
    let cache_path = models_dir().join("release-check.json");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cached: Option<serde_json::Value> =
        std::fs::read_to_string(&cache_path).ok().and_then(|text| serde_json::from_str(&text).ok());
    let fresh = cached
        .as_ref()
        .and_then(|c| c["checked_at"].as_u64())
        .is_some_and(|at| now.saturating_sub(at) < CHECK_INTERVAL_SECS);
    let latest: Vec<(Tool, Option<String>)> = if fresh {
        let cached = cached.unwrap_or_default();
        Tool::ALL
            .into_iter()
            .map(|tool| (tool, cached["latest"][tool.checkout_name()].as_str().map(String::from)))
            .collect()
    } else {
        let fetched: Vec<(Tool, Option<String>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = Tool::ALL.into_iter().map(|tool| (tool, scope.spawn(move || latest_release(tool)))).collect();
            handles.into_iter().map(|(tool, handle)| (tool, handle.join().ok().flatten())).collect()
        });
        // Record the attempt even when offline, so an unreachable GitHub costs at most
        // one short wait a day.
        let record = serde_json::json!({
            "checked_at": now,
            "latest": fetched
                .iter()
                .filter_map(|(tool, tag)| Some((tool.checkout_name().to_string(), serde_json::Value::from(tag.clone()?))))
                .collect::<serde_json::Map<_, _>>(),
        });
        let _ = std::fs::create_dir_all(models_dir());
        let _ = std::fs::write(&cache_path, record.to_string());
        fetched
    };
    latest.into_iter().filter_map(|(tool, tag)| Some((tool, tag?))).collect()
}

fn latest_release(tool: Tool) -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(CHECK_TIMEOUT)).build().into();
    let url = format!("https://api.github.com/repos/{}/releases/latest", tool.github());
    let body: serde_json::Value = agent
        .get(&url)
        .header("User-Agent", "forgeflow")
        .header("Accept", "application/vnd.github+json")
        .call()
        .ok()?
        .body_mut()
        .read_json()
        .ok()?;
    body["tag_name"].as_str().map(String::from)
}

fn install_hint(missing: &[&str]) -> String {
    if cfg!(target_os = "macos") {
        let mut steps = vec![];
        if missing.iter().any(|t| *t == "git" || *t == "c++") {
            steps.push("`xcode-select --install`".to_string());
        }
        if missing.contains(&"cmake") {
            steps.push("`brew install cmake`".to_string());
        }
        return format!("Install with {}.", steps.join(" and "));
    }
    let packages: Vec<&str> = missing
        .iter()
        .map(|t| match *t {
            "c++" => "build-essential",
            "nvcc" => "nvidia-cuda-toolkit",
            other => other,
        })
        .collect();
    format!("On Debian/Ubuntu: `sudo apt install {}`.", packages.join(" "))
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn command_output(program: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cuda_release_and_its_gcc_limit() {
        let version = "nvcc: NVIDIA (R) Cuda compiler driver\nCuda compilation tools, release 12.0, V12.0.140\n";
        assert_eq!(parse_cuda_version(version), Some((12, 0)));
        assert_eq!(max_gcc_for_cuda((12, 0)), 12);
        assert_eq!(max_gcc_for_cuda((12, 6)), 13);
        assert_eq!(max_gcc_for_cuda((13, 0)), 14);
    }

    #[test]
    fn targets_only_the_installed_cuda_architectures() {
        let gpu = Gpu { name: "RTX 3090".into(), count: 2, vram_bytes: 0, compute_caps: vec!["86".into(), "89".into()] };
        assert_eq!(cuda_architectures(&gpu).as_deref(), Some("-DCMAKE_CUDA_ARCHITECTURES=86;89"));
    }

    #[test]
    fn each_tool_has_a_flag_for_every_gpu_backend() {
        for tool in Tool::ALL {
            assert_eq!(tool.backend_flag(Backend::Cpu), None);
            for backend in [Backend::Metal, Backend::Cuda, Backend::Vulkan] {
                assert!(tool.backend_flag(backend).is_some());
            }
        }
        assert_eq!(Backend::parse(" CUDA "), Some(Backend::Cuda));
    }
}
