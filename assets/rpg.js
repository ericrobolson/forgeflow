// Enter to send, a growing text box, and following streamed narration.
(function () {
  const form = document.getElementById("composer");
  const message = document.getElementById("message");
  const log = document.getElementById("log");

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

  form.addEventListener("htmx:beforeRequest", () => {
    document.querySelector(".hello")?.remove();
    form.querySelector(".send").disabled = true;
  });
  form.addEventListener("htmx:afterRequest", (event) => {
    form.querySelector(".send").disabled = false;
    if (event.detail.successful) {
      message.value = "";
      grow();
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
