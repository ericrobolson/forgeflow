# ForgeFlow

Concatenative agentic orchestration workflow builder.


## Installation

Run `make install`, which builds and copies the binary to `~/bin/forgeflow`. (For a release build, run `cargo build --release` and copy `target/release/forgeflow` there instead.) After changing the source, run `make install` again; the `forgeflow` command runs that installed copy.
Then add 
```
forgeflow() { ~/bin/forgeflow $@ }
export -f forgeflow
```

to your zshrc

## Use

```
forgeflow              # chat in the current directory
forgeflow models       # local models, with fit and speed estimates for this machine
forgeflow pull <id>    # resumable download, sha256-verified, into ~/local-models
forgeflow use <id>     # select the model
forgeflow image        # image generation settings and local image models
forgeflow image <id>   # generate images locally with <id>, or `openrouter`
forgeflow edit <path>  # edit a file beside its pseudocode summary
forgeflow file.ff      # run a ForgeFlow program
```

The first time you run `forgeflow` with no model or `endpoint` configured, it walks you through choosing one: it lists models already in `~/local-models` and the recommended downloads for this machine, then downloads and selects the one you pick.

## Chat

Chat is turn by turn. In the terminal, Left and Right move the input cursor, Option+Left and Option+Right move by word on macOS, and Up and Down browse input history for the current session. Ordinary messages go directly to the selected model, with no tools available. Prefix a request with `;` (leading spaces are ignored) to translate it into a ForgeFlow program, type-check it, and run it:

```
; list files in src
```

Every other message stays in chat. ForgeFlow code can call its words directly, including `agent` when you want the model to use project tools:

```
> which lines of src/device.rs mention M4 Pro?
→ "src/device.rs" read_file "M4 Pro" grep print
94	        ("M4 Pro", 273.0),
translate 401 ms · run 0 ms
```

The chat assistant receives conversational history and no function tools. Semicolon-prefixed requests use a constrained translator; its programs are checked before running, and invalid programs get one correction attempt. Type `; help` to see the available vocabulary.

The code vocabulary includes file and directory reads, register memory (name a register as `R0` to `R15`, then use `@` to fetch and `!` to store), arithmetic, and `[ ... ] each` loops. Programs can also use `agent` to start the tool-calling loop. Ordinary chat messages never run these words implicitly.

### The stack

The stack persists between `;` lines, as in a Forth prompt, and whatever is left on it is shown after each program as a blue `stack:` line, bottom to top:

```
> ; 9 9 9
stack: 9 9 9
> ; dup
stack: 9 9 9 9
> ; + print
18
stack: 9 9 9
```

Programs are type-checked against the current stack, and the translator is told what is on it, so you can say "add them". A program that fails leaves the stack as it was. Long or multi-line values are shortened to one line in the display. `/clear` empties the stack along with the conversation.

### Suggest mode

Start a message with `?` to ask which commands to use for a goal. Nothing is run. The model is shown the current vocabulary, including project-defined words, and replies with one to three candidate commands; each is compiled and type-checked first, with one retry on errors, and invalid ones are dropped. The first valid command is placed on the next input line, so press Enter to run it or edit it first:

```
> ? how do i make an image
; "art/image.png" "an image" make-image
> ; "art/image.png" "an image" make-image█
```

If a word you need doesn't exist, or you ask how to define one, the suggestion is a definition line such as `; : double ( n Int -- result Int ) dup + ;`. It is checked before it is shown, and running it with Enter saves it to the word file (see below). If no word can do the job, the model says so, for example when searching for text across files, since there is no project-wide search word yet. The prefill works in the interactive terminal only. A bare `?` is an ordinary chat message.

### Project-defined words

Put word definitions in any `.ff` file under `_forgeflow/`; nested folders are searched too. Files are loaded shallowest first and then by path, and definitions can call words from any discovered file. The generated `_forgeflow/user_words.ff.txt` file is also loaded as ForgeFlow source and can be edited as plain text in Zed. (A file named `user_words.txt` from earlier versions is renamed to this automatically.)

In the interactive prompt there are two ways to add a word. Type a definition after the semicolon, e.g. `; : double ( n Int -- result Int ) dup + ;`; it is checked and saved to `_forgeflow/user_words.ff.txt` without calling the model, and `? how would i define ...` puts one on the input line for you. Or describe it in English with `; define ...`: ForgeFlow asks the model for one typed definition, validates it, saves it, and activates it immediately. For example, `; define a function called square which duplicates and multiplies its input` creates a persistent `square` word; call it with `; 3 square`. Invalid definitions are rejected without changing the saved file.

A definition has a checked stack signature:

```forth
: double ( n Int -- result Int ) $n $n + swap drop ;
```

The signature lists named inputs and outputs from bottom to top. `$n` pushes a copy of the named input; use normal stack words to consume or reorder values. Supported types are `String`, `Int`, `Bool`, `List`, `Register`, `Address`, and `Any`. Definitions may call one another. Recursive calls must be in tail position; final-word calls are trampolined. Words are available to `.ff` programs, translated `;` requests, and as agent tools. Use `/reload` in chat to validate and activate edits; an invalid reload keeps the working set.

Declare a durable named cell with `variable score`. It pushes an `Address` that can be saved in registers or passed to user-defined words. `@` fetches the cell and `!` stores into it, for example `10 score ! score @ print`. Named cell values are stored in `_forgeflow/memory.json`; register persistence remains separate.

## Editor

`forgeflow edit <path>`, or `/edit <path>` in chat, opens a file full screen with the text on the left and a high-level pseudocode summary on the right, written by the selected model with the file's real module, type, and function names. Each summary block sits beside the lines it describes, and the two panes scroll together; a short overview of the whole file runs across the top. It works on any text file: code in any language, configuration, or prose, where the summary gives the claims and definitions instead.

Keys: Ctrl+S saves, Ctrl+R regenerates the summary, Ctrl+O opens a fuzzy file picker, and Esc closes the editor (press it twice to discard unsaved changes). Option+Left and Option+Right move by word. A path that does not exist yet opens empty and is created on save.

A backtick (`` ` ``) switches to summary mode, where j and k (or Down and Up) jump to the next and previous block, and Left and Right step to the previous and next summary line, skipping blank ones. The selected summary line and its line number in the code are highlighted, and the cursor follows to that line. Entering summary mode on a line with no summary text selects the nearest one. A backtick, Enter, or Esc returns to editing at that line. Because the backtick switches modes, paste a literal one. Tests are listed by name only, one block per test case.

Summaries are cached in `_forgeflow/summaries/` (ignored by git), keyed by a SHA-256 of the file's contents together with the model and the prompt version, so reopening an unchanged file shows its summary at once without starting the model. After you edit, the summary stays until you save, and then it is regenerated. Each request starts from a fresh context and contains only the file. A file that fits in half the context window (`ctx`) is summarized in one request. A longer file is split by token count into overlapping windows, summarized in order with the names found so far carried forward, and then given one overview. When this machine could hold the whole file with a larger `ctx`, the footer says how large. Summaries that use names not found in the file get one correction request, and steps still naming them are dropped.

The `summarize` word returns the same summary as text, so `; summarize src/chat.rs` works in chat, and the agent can call it as a tool to read a summary instead of a whole file.

## Agent loop

The `agent` word (available in `.ff` programs) starts llama-server for the selected model (reusing one already serving it) and runs the loop in ForgeFlow itself. The model's tools are ForgeFlow words, with schemas generated from each word's typed inputs: `read_file`, `list_directory`, `summarize`, `reg_view`, `reg_search`, `reg_edit`, `reg_store`, and `emit`. File words are confined to the project root.

Tool results go into scratch registers (R4-R15), and the model receives a short receipt instead of the value, so small local models keep a small context. Text arguments accept `$R5` to pass a register's value. When scratch is full, the least recently used register is replaced and the receipt says so. Each run's transcript is saved in `_forgeflow/sessions/`.

`_forgeflow/config.json` holds the selected `model`, `image_model`, `ctx`, `port`, and `max_steps`. `image_model` defaults to `recraft/recraft-v4.1-flash`; `make-image` uses it with OpenRouter and reads the API key from `OPENROUTER_API_KEY`. For example, `"art/lantern.png" "A tiny brass fantasy lantern" make-image` generates one square image and returns its project-relative path. Pixel dimensions in the request are not honored; the model chooses its default resolution (Recraft produces roughly 1024×1024 images). Generated raster images are converted to PNG before saving. An agent run is limited to one image generation. To generate images locally instead, select a local image model with `forgeflow image <id>` (or `/image <id>` in chat); `forgeflow image` lists them, and `forgeflow image openrouter` switches back. This sets `image_backend` (`openrouter` or `local`) and `local_image_model` in the config. Local models download with `forgeflow pull <id>` and run through stable-diffusion.cpp's `sd-cli`, found at `~/local-models/stable-diffusion.cpp/build/bin/sd-cli`, on `PATH`, or via `FORGEFLOW_SD_CLI`; `forgeflow install` builds it (and llama-server) at a pinned release with the best GPU backend for the machine: Metal on Apple Silicon, CUDA with an NVIDIA card and toolkit, Vulkan, or CPU; set `FORGEFLOW_BUILD_BACKEND` to override. Startup warns when a newer release than the pinned one is out (checked at most daily; `FORGEFLOW_NO_UPDATE_CHECK=1` turns it off). The catalog has `z-image-turbo` (6.2G, 8 steps; about 3 minutes per 1024×1024 image on an M4 Pro) and `chroma1-hd` (10.2G, uncensored, 26 steps). To edit an existing image, `"shots/arena.png" "make it dusk and add rain" edit-image` saves `shots/arena-edit-1.png` (pass a list of up to 4 paths to give reference images; the first is edited). In chat, ask for the change and name the file. It runs the local edit model in `local_edit_model`, by default `qwen-image-edit-2511` (Qwen Image Edit 2511 Q4_K_M with the 4-step Lightning LoRA, 20.7G): `forgeflow pull qwen-image-edit-2511`, and `forgeflow image <id>` with an edit model's ID selects it. The output keeps the input's aspect ratio at about one megapixel. When GPU memory is short, the session's llama-server is stopped for the edit and restarts with the next message. To make a video, `"videos/fight.mp4" "shots/fight" "the second gladiator strides in" make-video` animates through keyframe images in order: a folder (its images in name order), comma-separated paths, or `""` for text alone. Each pair of neighbouring keyframes becomes a segment that starts on one and ends on the next (with VACE models the keyframes are passed as control frames), and the segments are joined. `"draft" video-quality` (the default) makes 512×512 segments for iterating; `"full" video-quality` runs at the model's resolution, then interpolates to twice the frame rate and upscales to 1080p with ffmpeg. `7 video-seconds` sets the length (0 for one 33-frame segment per keyframe pair): the length is split between keyframe pairs, and with fewer than two keyframes, segments are chained, each starting on the previous one's last frame (segments are at most 81 frames, so pairs of keyframes cap the length at about 5 s each). With only a starting image, characters drift over a long clip; an end keyframe holds them. `"videos/duel.mp4" "two gladiators duel…" draft-video` makes a draft from a prompt alone: the chat model plans the shot's keyframes (a start, an end, and one more per 5 seconds of `video-seconds`), the first is generated and the rest are edits of it with the edit model (so the characters, costumes and setting match, which separate generations would not), and the video is animated through them. The keyframes are saved beside the video as `duel-key-N.png` for checking, editing and reuse with `make-video`; without a chat model the prompt is used as written. In the web UI, Video mode at Draft with no photos attached uses it. `"videos/fight.mp4" finish-video` upscales an existing video to 1080p and doubles its frame rate in seconds, saving `fight-finished.mp4`: iterate on drafts, then finish the one you keep (the web UI has a Finish button under each video). Videos are saved as H.264 `.mp4` with a `.jpg` poster frame. The video model is `local_video_model` (default `wan2.2-vace-fun-nsfw`); `forgeflow image <id>` with a video model's ID selects it, and the catalog also has `wan2.1-vace-1.3b` and `wan2.2-ti2v-5b`. On an RTX 3090, a draft segment takes about 1.5 minutes with the A14B merge (1 minute with the smaller models), and a full 5-second clip about 7 minutes. Video models are run with `--offload-to-cpu --mmap`, so their weights may exceed system RAM. In the web UI, Video mode turns the attached photos into keyframes, with a Draft/Full switch. Catalog files can list `reshape` entries that relabel a GGUF tensor's shape after download, for conversions that store 5-D weights differently from what stable-diffusion.cpp expects. Some words need a person to point at something. `"shots/stunt.mp4" "Click the stunt performer" pick-points` opens a native window on the image or video (with a frame slider) where left-clicks mark what to include and right-clicks what to leave out; `pick-box` asks for a dragged box instead. Enter or Done returns the pick (frame time, image size, and points or box in source pixels) as JSON for the next word; Escape or Cancel stops the program. `"actors/lead.jpg" "Box the face" pick-box "actors/lead-face.png" crop-image` saves the boxed part. Each window runs in its own short-lived process (`forgeflow __pick`), since a process can create only one window event loop and words may run off the main thread; it needs a desktop session (X11 or Wayland). `"shots/stunt.mp4" "actors/lead-face.png" "shots/stunt-lead.mp4" swap-face` puts the face from an image onto a person in a video (or image), saving it at the output path. The target (the video or image to change) comes first, then the face image (png, jpg, webp — not a video), then the output path: a file path, a directory (generates `NAME-swapped.mp4` inside it), or `""` for `NAME-swapped` beside the target. It uses FaceFusion 3.9.2 (OpenRAIL-AS; its swap models are for non-commercial use), installed on first use into `~/local-models/facefusion` with its own Python environment and, with an NVIDIA GPU, the CUDA ONNX runtime (about 4 GB). Faces are sampled every half second: with one person in view it replaces every face; with more, a picker window shows the frame with the most faces, each outlined, and asks which to replace. The match distance is then set midway between how far that person's own face varies and the nearest other person, so turns and blur still match but nobody else changes. FaceFusion's own content check (which would refuse media it classes as explicit: more than 10% of frames, sampled once a second) is patched out during install, so all content is processed. Image, video and face-swap jobs pause the session's own llama-server when GPU memory is short; when the memory is held by something they cannot stop, such as another ForgeFlow session's model, they warn and name it. Videos are swapped at 512 px without a face enhancer, since the enhancer redraws detail independently each frame and visibly shimmers (measured +15% frame-to-frame change in the face against +3%); stills get the enhancer. Videos in other formats (webm, mov, mkv, avi) are converted to a temporary mp4 first, as FaceFusion writes the format it reads; the result is always `.mp4`. Prompts fit the model's context (`ctx`): each is built at the most detailed level that fits — every word described in full; the words most relevant to the message in full and the rest summarised; the most relevant in full and the rest as signatures; or, for very small contexts, only the most relevant words with compact instructions — keeping the worked examples most relevant to the message. The `;` translator uses the full, unchanging prompt whenever it fits, so llama-server reuses its cache. Chat leaves out the oldest turns when the conversation outgrows the context, and suggestion replies are capped so retries cannot overflow it. Set `endpoint` (and `endpoint_model`) to use another OpenAI-compatible server, such as Ollama, instead of llama-server.

Environment overrides: `FORGEFLOW_MODELS_DIR` (default `~/local-models`) and `FORGEFLOW_LLAMA_SERVER` (the llama-server binary).

### Manual face redraw and previews

When `swap-face` cannot detect a target face in a still image (for example, an anime character), it opens a native face editor. Box the face itself, then choose **Render**. The selected image-edit model uses the source face as an identity reference while preserving the target's drawing style. This fallback requires an installed local edit model (`forgeflow pull qwen-image-edit-2511` for the default). Videos without detectable faces require region tracking and are not supported by this fallback.

The window stays open during generation, showing a spinner, sampling progress, and live step images when enabled. Adjust the instruction, sampling steps, denoising strength, blend, feathering, face-shaped mask, or surrounding context, then Render again. **Show original** lets you compare the preview with the input. **Finish** saves the last completed render; Cancel or closing the window discards it. If another render fails, the previous completed preview remains available to finish.

The target crop is supplied as both an edit reference and the initial image, alongside a white-on-black mask passed to `sd-cli --mask`. The sampler preserves unmasked target latents during generation. **Denoising strength** (default `0.5`) controls how much the initial face can change; lower values also shorten the sampling schedule. **Edit blend** separately mixes the finished pixels with the original. With the four-step Lightning adapter, strength changes are coarse because they select from a short schedule.

Only pixels inside the selected face region are blended back into the original. The oval mask protects the corners; surrounding context gives the model more of the target image without enlarging the area that changes. Final compositing preserves pixels outside the mask exactly, even if VAE decoding changes the generated crop's surrounding pixels. Masked sampling constrains the edit but does not guarantee aligned facial features or recognizable identity; resemblance and style preservation depend on the edit model.

Selections, reference crops, intermediate step images, and completed previews stay in a unique job folder under `.tmp/.edits/` until the session finishes. Normal completion, cancellation, and errors remove that folder; an abrupt process kill or machine crash can leave it behind. Finished images are saved outside the job folder.

For reproducible headless checks, `cargo run --example masked_face_edit -- job.json` uses the same backend, mask, context crop and compositor as the editor. The JSON contains `target`, `reference`, `output` (a review directory), `target_box` and `reference_box` (`[x,y,width,height]`), `resolution`, `seed`, and `settings` (the serialized `FaceEditSettings`). It retains the input crops, mask, raw generated crop, final image, renderer log, and a pixel-preservation report in the review directory. Intermediate step images remain temporary. Test jobs using this session's images are under `.tmp/masked-face-test/`; these local files are not committed.

## Local Qwen LoRA training

CLI commands and words use the same dataset format and implementation. Start with:

```sh
forgeflow lora-init datasets/subject
# Put photographs in datasets/subject/images/ (subfolders also work).
forgeflow lora-scan datasets/subject
# Fill the filename captions in datasets/subject/lora.json.
forgeflow lora-check datasets/subject
forgeflow train-lora datasets/subject
forgeflow lora datasets/subject 0.8
```

`lora-init` creates `images/`, `lora.json`, and `README.txt`. It refuses to overwrite an existing manifest. `lora-scan` adds newly found JPEG, PNG, and WebP filenames with empty captions, preserving your existing captions and settings. It never deletes entries for missing images; validation reports them so they can be corrected or removed deliberately. Filename keys are relative to `images/`.

Normal startup is unchanged: start `forgeflow` as usual and use the words in that session or in `.ff` workflows. The CLI aliases are optional conveniences. `train-lora` prepares any missing model components automatically; `use-lora` saves the compatible local model and adapter selection for later sessions. There are no new startup flags. Managed training currently targets Linux with an NVIDIA CUDA GPU.

The generated manifest is a JSON object like this:

```json
{
  "version": 1,
  "name": "subject",
  "base_model": "qwen-image",
  "trigger": "ff_subject",
  "images": {
    "front.jpg": "a person, front view, neutral expression, daylight",
    "portraits/side.webp": ["a person", "side view", "studio lighting"]
  },
  "training": {
    "steps": 1000,
    "rank": 16,
    "learning_rate": 0.0001,
    "resolution": 512,
    "save_every": 100,
    "seed": 42
  }
}
```

Captions can be strings or arrays of phrase tags. Training joins tag arrays with commas and prepends the trigger if it is absent. Empty captions, missing/unreadable images, escaping paths and invalid settings fail before training. Only listed images are trained on. Describe variable attributes such as clothing, lighting and pose in the captions, and use varied examples of the subject or style you want the trigger to learn.

The corresponding words are:

```forth
"datasets/subject" lora-init print
"datasets/subject" lora-scan print
"datasets/subject" lora-check print
"datasets/subject" lora-prepare print
"datasets/subject" train-lora print
"datasets/subject" "0.8" use-lora
"art/portrait.png" "a studio portrait" make-image
"off" "" use-lora
```

`lora-prepare` validates the dataset and returns a frozen run folder without installing a trainer or starting GPU work. `train-lora` creates a new run under `datasets/subject/runs/`, with normalized image copies, caption files, the exact dataset JSON, trainer configuration, logs and checkpoints. Source photographs are unchanged. Each run is separate; changing captions or images while one is running does not change that run. Failed runs retain their diagnostics. The final weights path is returned, and `adapter.json` in the dataset points to the latest successful result.

Training uses a pinned [AI Toolkit](https://github.com/ostris/ai-toolkit) revision in an isolated Python environment under `~/local-models/ai-toolkit/`. Its Qwen 24 GB recipe uses rank-limited LoRA training, quantization, cached text embeddings, gradient checkpointing and batch size one. On Linux with a user systemd session, the trainer is placed in a separate scope limited to 75% of system RAM and 512 MiB swap. The session's owned chat model is paused when GPU memory is needed; concurrent ForgeFlow training jobs are rejected. Other environments rely on the trainer's memory settings.

This trains **Qwen Image's text-to-image base**, not the Qwen Image Edit model. The existing edit-model GGUF is an inference artifact and cannot be used directly by this trainer. `forgeflow pull qwen-image` downloads a roughly **38 GiB BF16 transformer**; encoders and VAE already used by ForgeFlow are reused for inference. Training additionally downloads roughly **16 GiB** of text-encoder/VAE components and Python dependencies. The generation transformer is shared with training instead of downloaded twice. These are persistent model assets; dataset scaffolding, scanning, validation and preparation do not download them.

`forgeflow lora <dataset> [strength]` / `use-lora` selects the saved adapter and switches local generation to its compatible Qwen base. Its trigger is added to generation prompts automatically. Strength defaults to `1`; `forgeflow lora off` disables it. Switching to an incompatible local image model while an adapter is selected returns an actionable error. The adapter is applied to local text-to-image generation; image editing remains separate.

## Anime rendering

`anime-style` renders a whole image or every frame of an existing video:

```forth
"photos/action.jpg" "photos/action-anime.png" "hayao" anime-style
"videos/fight.webm" "videos/fight-anime.mp4" "arcane" anime-style
"videos/fight.mov" "" "" anime-style
```

Arguments are **input, output, style**; the word returns the project-relative saved path and displays it in the web UI. Blank style selects `hayao`. Style names are case-insensitive; an unrecognized name returns every name and description. All output filenames receive the same local `YYMMDDhhmmss-` prefix as `swap-face`. For example, requesting `fight-anime.mp4` saves `261111153000-fight-anime.mp4`. Blank output generates `NAME-anime.png` or `NAME-anime.mp4` beside the input; a directory or path ending in `/` generates it inside that directory. An existing timestamped destination is refused rather than overwritten; choose another name or retry a second later.

| Style | Rendering | Model |
| --- | --- | --- |
| `hayao` | Soft painted colors and anime shading; whole-frame default | AnimeGANv3 Hayao |
| `shinkai` | Vivid colors and detailed anime scenery | AnimeGANv3 Shinkai |
| `arcane` | Painterly Arcane rendering; strongest on portraits | ArcaneGAN v0.4 by Alex Spirin, **not** the unavailable AnimeGANv3 Arcane checkpoint |
| `8bit` | Four-pixel grid and 32 colors, with a palette fixed for each clip | Hayao base plus pixel-art rendering; an alternative to AnimeGANv3 8bit |
| `comic` | Ink outlines and posterized shading | Hayao base plus comic rendering; an alternative to AnimeGANv3 Comic |
| `ghibli` | Ghibli-c1 painted character rendering; strongest on large faces | AnimeGANv3 Ghibli-c1 portrait model |
| `sketch` | Drawn linework and sketch shading | AnimeGANv3 Portrait Sketch |
| `cute` | Simplified, playful cartoon faces | AnimeGANv3 tiny Cute portrait model |
| `jp-face` | Japanese anime faces; strongest on large faces | AnimeGANv3 JP Face portrait model |

Every listed style works without supplying weights manually. Model files download only when needed, with SHA-256 verification. Portrait models can perform poorly on small faces in full-body action; all these styles process the entire frame. There is no masking or temporal stabilization in this version.

Images can be saved as PNG, JPEG, or WebP; videos as MP4, WebM, MOV, or MKV. Animated GIFs are processed as videos. Frames are streamed through the model with their original timestamps, including variable frame durations, and the source audio is restored. Rendering reports estimated completion percentages from source timestamps/duration (or frame count), rendering advances to an estimated 95%, followed by encoding/audio work and 100% after the result is saved. Odd video dimensions gain one padding pixel for encoding. Image orientation and transparency are retained where the output format supports them. HDR video goes through 8-bit RGB.

First use automatically installs an isolated Python environment under `~/local-models/anime-style/`, or `$FORGEFLOW_MODELS_DIR/anime-style/`. Requires Python 3.10 or newer with `venv`, `ffmpeg`, and `ffprobe`. NVIDIA machines use a pinned CUDA ONNX runtime and its CUDA/cuDNN libraries (about 3.4 GB here); other machines use the CPU runtime. Runtime imports and model checks happen automatically, concurrent installations are locked, and interrupted installations can be retried. A healthy installation runs without pip or downloads. The six AnimeGANv3 checkpoints are approximately 2.5–7 MB each; `8bit` and `comic` reuse Hayao.

ArcaneGAN's official checkpoint is TorchScript (about 63 MB). Its first use additionally installs pinned CPU PyTorch and ONNX export dependencies and converts it once to an ONNX model locally. Subsequent jobs run through the shared ONNX runtime, including CUDA when available. The source checkpoint and derived ONNX model are checksum validated, so interrupted or damaged conversions are rebuilt automatically. The extra exporter packages remain isolated in this environment.

Model sources: [AnimeGANv3 v1.1.0](https://github.com/TachibanaYoshino/AnimeGANv3/releases/tag/v1.1.0), [AnimeGANv3 portrait releases](https://github.com/TachibanaYoshino/AnimeGANv3_Portrait_Inference/releases/tag/1.0), [AnimeGANv3 GUI releases](https://github.com/TachibanaYoshino/AnimeGANv3_gui.exe/releases/tag/V1.1), and [ArcaneGAN v0.4](https://github.com/Sxela/ArcaneGAN/releases/tag/v0.4). AnimeGANv3 has a [non-commercial license](https://github.com/TachibanaYoshino/AnimeGANv3#scroll-license); ArcaneGAN is [MIT licensed](https://github.com/Sxela/ArcaneGAN/blob/main/LICENSE).

Installer recovery tests: `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p '*_test.py'`. Compile and word tests: `cargo test`.
