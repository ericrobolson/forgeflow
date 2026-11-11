// The few behaviours htmx does not cover: Enter to send, a growing text box, a preview of
// the attached photo, and keeping the newest message in view.
(function () {
  const form = document.getElementById("composer");
  const message = document.getElementById("message");
  const photo = document.getElementById("photo");
  const preview = document.getElementById("preview");
  const log = document.getElementById("log");

  // The placeholder says what the selected mode does.
  const placeholders = {
    chat: "Message",
    code: "Program, such as 2 3 + print",
    image: "Describe an image, or attach a photo and say what to change",
    video: "Describe the motion; attached photos become keyframes, in order",
  };
  const showMode = () => {
    const mode = form.querySelector('input[name="mode"]:checked')?.value || "chat";
    message.placeholder = placeholders[mode];
  };
  form.querySelectorAll('input[name="mode"]').forEach((radio) => radio.addEventListener("change", () => { showMode(); message.focus(); }));
  showMode();

  const nearBottom = () => log.scrollHeight - log.scrollTop - log.clientHeight < 120;
  const toBottom = () => { log.scrollTop = log.scrollHeight; };
  toBottom();

  const grow = () => {
    message.style.height = "auto";
    message.style.height = message.scrollHeight + "px";
  };
  message.addEventListener("input", grow);

  // Enter sends on a keyboard; Shift+Enter adds a line. Phones use the send key.
  message.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing && matchMedia("(pointer: fine)").matches) {
      event.preventDefault();
      form.requestSubmit();
    }
  });

  const clearPhoto = () => { photo.value = ""; preview.replaceChildren(); };
  photo.addEventListener("change", () => {
    preview.replaceChildren();
    const files = [...photo.files];
    if (!files.length) return;
    for (const file of files) {
      const image = document.createElement("img");
      image.src = URL.createObjectURL(file);
      image.title = file.name;
      preview.append(image);
    }
    const name = document.createElement("span");
    name.className = "count";
    name.textContent = files.length === 1 ? files[0].name : `${files.length} photos`;
    const remove = document.createElement("button");
    remove.type = "button";
    remove.textContent = "×";
    remove.title = "Remove photos";
    remove.addEventListener("click", clearPhoto);
    preview.append(name, remove);
    message.focus();
  });

  form.addEventListener("htmx:beforeRequest", () => {
    document.querySelector(".hello")?.remove();
    form.querySelector(".send").disabled = true;
  });
  form.addEventListener("htmx:afterRequest", (event) => {
    form.querySelector(".send").disabled = false;
    if (event.detail.successful) {
      message.value = "";
      grow();
      clearPhoto();
    }
    toBottom();
  });

  // Follow a streaming reply only while the reader is at the bottom.
  document.body.addEventListener("htmx:sseBeforeMessage", () => {
    const follow = nearBottom();
    requestAnimationFrame(() => { if (follow) toBottom(); });
  });
  // Images load after they are inserted and push the page down.
  log.addEventListener("load", (event) => { if (event.target.tagName === "IMG" && nearBottom()) toBottom(); }, true);
})();
