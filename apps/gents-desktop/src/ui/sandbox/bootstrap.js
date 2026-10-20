/* The one inline script in a plugin frame. The frame's origin is opaque, so
   it can fetch nothing from the shell: the runtime arrives in the shell's
   connect message, and the frame mints its own blob for it. A frame written
   with srcdoc inherits the shell's policy, which admits this text by hash
   (`'sha256-…'` in tauri.conf.json script-src) and the blob by scheme; the
   frame's own policy admits the same two. Change a byte here and the hash
   test tells you to re-pin the config. */
(function () {
  "use strict";
  var taken = false;
  function connect(event) {
    if (taken || event.source !== window.parent) return;
    var data = event.data;
    var port = event.ports && event.ports[0];
    if (!data || data.t !== "connect" || !port || typeof data.runtime !== "string")
      return;
    taken = true;
    window.removeEventListener("message", connect);
    window.__gentsFrame = { port: port };
    var url = URL.createObjectURL(
      new Blob([data.runtime], { type: "text/javascript" }),
    );
    var script = document.createElement("script");
    script.src = url;
    script.onload = function () {
      URL.revokeObjectURL(url);
    };
    script.onerror = function () {
      URL.revokeObjectURL(url);
      port.postMessage({
        t: "failed",
        error: "the sandbox runtime did not load in the frame",
      });
    };
    document.head.appendChild(script);
  }
  window.addEventListener("message", connect);
})();
