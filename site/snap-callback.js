// Shows the Snapchat sign-in result from this page's address as "<state>.<code>" for the owner
// to paste into Shorts Circuit. It makes no requests (the page's CSP forbids them anyway).
"use strict";

(function () {
  var params = new URLSearchParams(window.location.search);
  var state = params.get("state") || "";
  var code = params.get("code") || "";
  var error = params.get("error") || "";
  var description = params.get("error_description") || "";

  // Keep the one-time code out of the address bar and the browser history.
  if (window.location.search && window.history && window.history.replaceState) {
    window.history.replaceState(null, "", window.location.pathname);
  }

  function show(id) {
    document.getElementById(id).hidden = false;
  }

  var safe = /^[A-Za-z0-9._~+\/=-]{1,2048}$/;

  if (error) {
    document.getElementById("reason").textContent = (description || error).slice(0, 500);
    show("failed");
    return;
  }
  if (!code) {
    show("empty");
    return;
  }
  if (!safe.test(code) || (state && !safe.test(state))) {
    document.getElementById("reason").textContent = "the page's address doesn't look like a sign-in result.";
    show("failed");
    return;
  }

  var text = state ? state + "." + code : code;
  var box = document.getElementById("code");
  var status = document.getElementById("copied");
  box.value = text;
  show("result");
  box.addEventListener("focus", function () {
    box.select();
  });

  document.getElementById("copy").addEventListener("click", function () {
    function selected() {
      box.focus();
      box.select();
      try {
        if (document.execCommand("copy")) {
          status.textContent = "Copied. Paste it into Shorts Circuit.";
          return;
        }
      } catch (e) {
        // fall through
      }
      status.textContent = "Press Ctrl+C (or ⌘C) to copy the selected text.";
    }
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(function () {
        status.textContent = "Copied. Paste it into Shorts Circuit.";
      }, selected);
    } else {
      selected();
    }
  });
})();
