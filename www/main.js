/* ============================================================================
   LOOMWEAVE — site interactions (progressive enhancement only)
   ----------------------------------------------------------------------------
   The site is fully content-complete with JavaScript disabled: every code block
   is selectable text, every nav link is a real anchor, every table renders
   server-side. This script adds ONE convenience — copy-to-clipboard on code
   blocks — and nothing the page depends on. Copy buttons are injected here (not
   in markup) so a JS-off visitor never sees a dead button.
   ============================================================================ */
(function () {
  "use strict";

  if (!navigator.clipboard) return; // no Clipboard API → leave the text selectable

  var blocks = Array.prototype.slice.call(document.querySelectorAll(".code"));

  blocks.forEach(function (block) {
    var pre = block.querySelector("pre");
    var head = block.querySelector(".code-head");
    if (!pre || !head || head.querySelector(".copy-btn")) return;

    var btn = document.createElement("button");
    btn.type = "button";
    btn.className = "copy-btn";
    btn.textContent = "Copy";
    btn.setAttribute("aria-label", "Copy code to clipboard");

    btn.addEventListener("click", function () {
      // Copy the visible text, stripping a leading shell "$ " prompt per line
      // so a pasted command runs cleanly.
      var text = pre.innerText.replace(/^\$ /gm, "");
      navigator.clipboard.writeText(text).then(function () {
        btn.textContent = "Copied";
        btn.classList.add("copied");
        setTimeout(function () {
          btn.textContent = "Copy";
          btn.classList.remove("copied");
        }, 1600);
      });
    });

    head.appendChild(btn);
  });
})();
