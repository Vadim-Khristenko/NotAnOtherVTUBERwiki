// The picture viewer: a click on an article's picture or diagram opens it over
// the page, where it zooms, pans, rotates and goes full screen. Without this
// script the same click follows the picture's link: a file to its file page,
// a diagram to its SVG. The markup and its labels come from the skin's
// <template id="viewer-template">, so the words stay translated.
(function () {
  "use strict";
  var template = document.getElementById("viewer-template");
  var content = document.getElementById("content");
  if (!template || !content) return;

  var MIN = 0.05;
  var MAX = 16;
  var STEP = 1.25;

  var root = null;
  var img = null;
  var zoomOut = null;
  var state = { scale: 1, x: 0, y: 0, rotate: 0, fit: 1 };
  var opener = null;
  var pointers = new Map();
  var pinch = null;
  var drag = null;

  // What a click opens, or null when it is not a picture this viewer shows.
  function target(el) {
    if (!el || el.tagName !== "IMG" || !content.contains(el)) return null;
    if (el.classList.contains("emote") || el.closest(".img-missing, .page-actions, nav")) return null;
    var src = el.currentSrc || el.getAttribute("src") || "";
    if (!src) return null;
    var diagram = el.closest("figure.diagram");
    var link = el.closest("a");
    var fileLink = el.closest("a.file-link");
    // A picture linking somewhere else (a page, a site) keeps its link.
    if (link && !fileLink && !link.classList.contains("diagram-open")) return null;
    return {
      src: src,
      alt: el.getAttribute("alt") || "",
      file: fileLink ? fileLink.getAttribute("href") : null,
      original: diagram && link ? link.getAttribute("href") : src,
      vector: !!diagram || /\.svg($|\?)/i.test(src),
      width: el.naturalWidth || el.width,
      height: el.naturalHeight || el.height,
    };
  }

  function build() {
    root = template.content.firstElementChild.cloneNode(true);
    img = root.querySelector(".viewer-img");
    zoomOut = root.querySelector(".viewer-zoom");
    root.addEventListener("click", onClick);
    root.addEventListener("wheel", onWheel, { passive: false });
    var stage = root.querySelector(".viewer-stage");
    stage.addEventListener("pointerdown", onDown);
    stage.addEventListener("pointermove", onMove);
    stage.addEventListener("pointerup", onUp);
    stage.addEventListener("pointercancel", onUp);
    stage.addEventListener("dblclick", onDouble);
    // iPhone Safari has no full screen for a page element.
    if (!root.requestFullscreen) root.querySelector('[data-act="fullscreen"]').hidden = true;
    document.body.appendChild(root);
  }

  function open(t, from) {
    if (!root) build();
    opener = from;
    img.alt = t.alt;
    img.src = t.src;
    root.querySelector(".viewer-caption").textContent = t.alt;
    var file = root.querySelector(".viewer-file");
    file.hidden = !t.file;
    if (t.file) file.href = t.file;
    root.querySelector(".viewer-original").href = t.original;
    root.hidden = false;
    document.documentElement.classList.add("viewer-open");
    var ready = function () {
      state.rotate = 0;
      fit(t.vector);
    };
    if (img.complete && img.naturalWidth) ready();
    else img.addEventListener("load", ready, { once: true });
    root.querySelector('[data-act="close"]').focus();
    document.addEventListener("keydown", onKey);
  }

  function close() {
    if (!root || root.hidden) return;
    if (document.fullscreenElement === root) document.exitFullscreen().catch(function () {});
    root.hidden = true;
    img.removeAttribute("src");
    pointers.clear();
    pinch = drag = null;
    document.documentElement.classList.remove("viewer-open");
    document.removeEventListener("keydown", onKey);
    if (opener && opener.focus) opener.focus();
  }

  // Scale that shows the whole picture; a vector drawing may grow past its
  // own size, a photo only shrinks.
  function fit(vector) {
    var stage = root.querySelector(".viewer-stage").getBoundingClientRect();
    var w = img.naturalWidth || 1;
    var h = img.naturalHeight || 1;
    if (state.rotate % 180 !== 0) {
      var t = w;
      w = h;
      h = t;
    }
    // The toolbar floats over the bottom of the stage; keep the picture clear of it.
    var bar = root.querySelector(".viewer-bar").getBoundingClientRect().height + 32;
    var s = Math.min((stage.width * 0.94) / w, (stage.height - bar * 2) / h);
    if (!vector) s = Math.min(s, 1);
    state.fit = s;
    state.scale = s;
    state.x = 0;
    state.y = 0;
    apply();
  }

  function apply() {
    img.style.transform =
      "translate(-50%, -50%) translate(" + state.x + "px, " + state.y + "px) rotate(" + state.rotate + "deg) scale(" + state.scale + ")";
    zoomOut.textContent = Math.round(state.scale * 100) + "%";
  }

  // Zooms by `factor` keeping the point (cx, cy), relative to the stage
  // centre, where it is.
  function zoom(factor, cx, cy) {
    var next = Math.min(MAX, Math.max(MIN, state.scale * factor));
    var k = next / state.scale;
    cx = cx || 0;
    cy = cy || 0;
    state.x = cx - (cx - state.x) * k;
    state.y = cy - (cy - state.y) * k;
    state.scale = next;
    apply();
  }

  function centre(event) {
    var r = root.querySelector(".viewer-stage").getBoundingClientRect();
    return { x: event.clientX - r.left - r.width / 2, y: event.clientY - r.top - r.height / 2 };
  }

  function act(name) {
    switch (name) {
      case "zoom-in":
        zoom(STEP);
        break;
      case "zoom-out":
        zoom(1 / STEP);
        break;
      case "reset":
        state.rotate = 0;
        fit(true);
        break;
      case "rotate-left":
        state.rotate -= 90;
        apply();
        break;
      case "rotate-right":
        state.rotate += 90;
        apply();
        break;
      case "fullscreen":
        if (document.fullscreenElement) document.exitFullscreen().catch(function () {});
        else if (root.requestFullscreen) root.requestFullscreen().catch(function () {});
        break;
      case "close":
        close();
        break;
    }
  }

  function onClick(event) {
    var button = event.target.closest("[data-act]");
    if (button) {
      event.preventDefault();
      act(button.getAttribute("data-act"));
      return;
    }
    // A click on the dark backdrop, not on the picture or a control, closes.
    if (event.target.classList.contains("viewer-stage") && !drag) close();
  }

  function onWheel(event) {
    if (!event.target.closest(".viewer-stage")) return;
    event.preventDefault();
    var c = centre(event);
    zoom(event.deltaY < 0 ? 1.15 : 1 / 1.15, c.x, c.y);
  }

  function onDown(event) {
    if (event.button !== 0 && event.pointerType === "mouse") return;
    event.currentTarget.setPointerCapture(event.pointerId);
    pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    if (pointers.size === 2) {
      var p = Array.from(pointers.values());
      pinch = { dist: Math.hypot(p[0].x - p[1].x, p[0].y - p[1].y), scale: state.scale };
      drag = null;
    } else {
      drag = { x: event.clientX, y: event.clientY, ox: state.x, oy: state.y, moved: false };
    }
  }

  function onMove(event) {
    if (!pointers.has(event.pointerId)) return;
    pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    if (pinch && pointers.size === 2) {
      var p = Array.from(pointers.values());
      var dist = Math.hypot(p[0].x - p[1].x, p[0].y - p[1].y);
      var mid = centre({ clientX: (p[0].x + p[1].x) / 2, clientY: (p[0].y + p[1].y) / 2 });
      zoom((pinch.scale * (dist / pinch.dist)) / state.scale, mid.x, mid.y);
      return;
    }
    if (drag) {
      var dx = event.clientX - drag.x;
      var dy = event.clientY - drag.y;
      if (Math.abs(dx) + Math.abs(dy) > 3) drag.moved = true;
      state.x = drag.ox + dx;
      state.y = drag.oy + dy;
      apply();
    }
  }

  function onUp(event) {
    pointers.delete(event.pointerId);
    if (pointers.size < 2) pinch = null;
    // A drag that moved must not count as a backdrop click afterwards.
    var moved = drag && drag.moved;
    drag = null;
    if (moved) {
      drag = { moved: true };
      setTimeout(function () {
        drag = null;
      }, 0);
    }
  }

  function onDouble(event) {
    var c = centre(event);
    if (state.scale > state.fit * 1.05) fit(true);
    else zoom(2.5, c.x, c.y);
  }

  function onKey(event) {
    var pan = 60;
    switch (event.key) {
      case "Escape":
        close();
        break;
      case "+":
      case "=":
        zoom(STEP);
        break;
      case "-":
        zoom(1 / STEP);
        break;
      case "0":
        act("reset");
        break;
      case "r":
        act(event.shiftKey ? "rotate-left" : "rotate-right");
        break;
      case "R":
        act("rotate-left");
        break;
      case "f":
        act("fullscreen");
        break;
      case "ArrowLeft":
        state.x += pan;
        apply();
        break;
      case "ArrowRight":
        state.x -= pan;
        apply();
        break;
      case "ArrowUp":
        state.y += pan;
        apply();
        break;
      case "ArrowDown":
        state.y -= pan;
        apply();
        break;
      case "Tab":
        trapFocus(event);
        return;
      default:
        return;
    }
    event.preventDefault();
  }

  // Keyboard focus stays inside the open viewer.
  function trapFocus(event) {
    var items = Array.from(root.querySelectorAll("button, a[href]")).filter(function (el) {
      return !el.hidden && el.offsetParent !== null;
    });
    if (!items.length) return;
    var first = items[0];
    var last = items[items.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  content.addEventListener("click", function (event) {
    if (event.defaultPrevented || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
    var t = target(event.target);
    if (!t) return;
    event.preventDefault();
    open(t, event.target.closest("a") || event.target);
  });

  // Pictures the viewer opens say so to the pointer.
  content.querySelectorAll("img").forEach(function (el) {
    if (target(el)) el.classList.add("viewer-zoomable");
  });
})();
