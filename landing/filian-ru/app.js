// filian.ru enhancements. The page is complete without this file: everything here
// is either a convenience (clock, draggable windows, remembered language) or a joke.
(function () {
  "use strict";

  var root = document.documentElement;
  var reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  var desktopMode = window.matchMedia("(min-width: 1100px) and (pointer: fine)");

  // Storage can throw in private windows or with blocked site data, so every
  // access goes through these and failure just means "not remembered".
  function load(key) { try { return localStorage.getItem("filianru." + key); } catch (e) { return null; } }
  function save(key, value) { try { localStorage.setItem("filianru." + key, value); } catch (e) {} }

  function $(sel, ctx) { return (ctx || document).querySelector(sel); }
  function $all(sel, ctx) { return Array.prototype.slice.call((ctx || document).querySelectorAll(sel)); }

  /* ---------- Language ---------- */
  var langBox = $("#lang-en");
  var TITLES = {
    ru: "filian.ru: Snackers RU, русскоязычные фанаты Filian",
    en: "filian.ru: Snackers RU, Russian-speaking Filian fans"
  };
  function applyLang() {
    var lang = langBox.checked ? "en" : "ru";
    root.lang = lang;
    document.title = TITLES[lang];
    save("lang", lang);
    $all("[data-label-ru]").forEach(function (el) {
      el.setAttribute("aria-label", el.getAttribute("data-label-" + lang));
    });
  }
  if (load("lang") === "en") langBox.checked = true;
  langBox.addEventListener("change", applyLang);
  applyLang();
  function isEn() { return langBox.checked; }

  /* ---------- Colour scheme ---------- */
  var themeBtn = $("#theme-btn");
  if (themeBtn) {
    themeBtn.addEventListener("click", function () {
      var current = root.dataset.theme ||
        (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
      var next = current === "dark" ? "light" : "dark";
      root.dataset.theme = next;
      save("theme", next);
      closeStart();
    });
  }

  /* ---------- CRT effect ----------
     The checkbox works on its own through CSS. This only remembers the choice and
     drops the class the head script set, so the two never disagree. */
  var crt = $("#crt-on");
  if (crt) {
    if (load("crt") === "off") crt.checked = false;
    crt.addEventListener("change", function () {
      root.classList.toggle("crt-off", !crt.checked);
      save("crt", crt.checked ? "on" : "off");
    });
  }

  /* ---------- Clock ---------- */
  var clock = $("#clock");
  function tick() {
    var d = new Date();
    clock.textContent = String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
  }
  if (clock) { tick(); setInterval(tick, 10000); }

  /* ---------- Tips ---------- */
  var tips = $all(".tips li");
  var tipIndex = 0;
  var nextTip = $("#next-tip");
  if (nextTip && tips.length) {
    nextTip.addEventListener("click", function () {
      tips[tipIndex].classList.remove("is-current");
      tipIndex = (tipIndex + 1) % tips.length;
      tips[tipIndex].classList.add("is-current");
    });
  }

  /* ---------- Start menu ---------- */
  var start = $("#start");
  function closeStart() { if (start && start.open) start.open = false; }
  document.addEventListener("click", function (e) {
    if (start && start.open && !start.contains(e.target)) closeStart();
  });
  document.addEventListener("keydown", function (e) {
    if (e.key === "Escape" && start && start.open) {
      closeStart();
      $("summary", start).focus();
    }
  });
  $all(".start__menu a").forEach(function (a) { a.addEventListener("click", closeStart); });

  /* ---------- Taskbar: mark the window currently in view ---------- */
  var tasks = $all(".tasks a[data-task]");
  var wins = tasks.map(function (a) { return document.getElementById(a.dataset.task); });
  if ("IntersectionObserver" in window) {
    var visible = new Map();
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (en) { visible.set(en.target.id, en.intersectionRatio); });
      var best = null, bestRatio = 0;
      visible.forEach(function (r, id) { if (r > bestRatio) { bestRatio = r; best = id; } });
      tasks.forEach(function (a) {
        if (a.dataset.task === best) a.setAttribute("aria-current", "true");
        else a.removeAttribute("aria-current");
      });
    }, { threshold: [0, 0.25, 0.5, 0.75, 1] });
    wins.forEach(function (w) { if (w) io.observe(w); });
  }

  /* ---------- Windows: focus, minimise, drag (desktop only) ---------- */
  var topZ = 2;
  function activate(win) {
    // Active and inactive title bars only mean something when windows can overlap.
    if (!desktopMode.matches) return;
    $all("main .win").forEach(function (w) { w.classList.toggle("is-inactive", w !== win); });
    topZ += 1;
    win.style.zIndex = topZ;
  }

  // The outline that flies between a window and its taskbar button, like Win95 did.
  // It explains where a minimised window went, so it is worth its 180ms.
  function zoom(fromEl, toEl, done) {
    if (reduceMotion.matches || !fromEl || !toEl || !Element.prototype.animate) { done(); return; }
    var a = fromEl.getBoundingClientRect(), b = toEl.getBoundingClientRect();
    var r = document.createElement("div");
    r.className = "zoom-rect";
    r.style.left = a.left + "px"; r.style.top = a.top + "px";
    r.style.width = a.width + "px"; r.style.height = a.height + "px";
    r.style.transformOrigin = "0 0";
    document.body.appendChild(r);
    var anim = r.animate([
      { transform: "translate(0,0) scale(1,1)" },
      { transform: "translate(" + (b.left - a.left) + "px," + (b.top - a.top) + "px) scale(" + (b.width / a.width) + "," + (b.height / a.height) + ")" }
    ], { duration: 180, easing: "steps(6)" });
    anim.onfinish = function () { r.remove(); done(); };
  }

  function taskFor(win) { return $('.tasks a[data-task="' + win.id + '"]'); }

  function minimise(win) {
    var task = taskFor(win);
    zoom(win, task, function () {
      win.hidden = true;
      if (task) { task.classList.add("is-min"); task.focus(); }
    });
  }
  function closeWin(win) {
    var task = taskFor(win);
    win.hidden = true;
    if (task) { task.classList.add("is-closed"); task.focus(); }
  }
  function restore(win) {
    var task = taskFor(win);
    win.hidden = false;
    if (task) task.classList.remove("is-min", "is-closed");
    activate(win);
  }

  // Any in-page link to a window brings it back first: desktop icons, the Start
  // menu, taskbar buttons, the footer. Restoring before the browser follows the
  // link means the normal anchor scroll still lands on it.
  function reopenFromHash(hash) {
    if (!hash || hash.length < 2) return;
    var win = document.getElementById(decodeURIComponent(hash.slice(1)));
    if (win && win.hidden && win.classList.contains("win")) restore(win);
    else if (win && win.classList.contains("win")) activate(win);
  }
  document.addEventListener("click", function (e) {
    var a = e.target.closest && e.target.closest("a[href^=\"#\"]");
    if (a) reopenFromHash(a.getAttribute("href"));
  }, true);
  window.addEventListener("hashchange", function () { reopenFromHash(location.hash); });

  function makeControls(win) {
    var bar = $(".titlebar", win);
    if (!bar || $(".titlebar__ctrl", bar)) return;
    var ctrl = document.createElement("span");
    ctrl.className = "titlebar__ctrl";
    var min = document.createElement("button");
    min.type = "button";
    min.className = "tbtn tbtn--min";
    min.setAttribute("data-label-ru", "Свернуть");
    min.setAttribute("data-label-en", "Minimise");
    min.setAttribute("aria-label", isEn() ? "Minimise" : "Свернуть");
    min.addEventListener("click", function () { minimise(win); });
    var cls = document.createElement("button");
    cls.type = "button";
    cls.className = "tbtn tbtn--close";
    cls.setAttribute("data-label-ru", "Закрыть");
    cls.setAttribute("data-label-en", "Close");
    cls.setAttribute("aria-label", isEn() ? "Close" : "Закрыть");
    cls.addEventListener("click", function () { closeWin(win); });
    ctrl.appendChild(min);
    ctrl.appendChild(cls);
    bar.appendChild(ctrl);
  }

  function makeDraggable(win) {
    var bar = $(".titlebar", win);
    var sx = 0, sy = 0, ox = 0, oy = 0, dragging = false;
    win.classList.add("is-draggable");
    bar.addEventListener("pointerdown", function (e) {
      if (!desktopMode.matches || e.button !== 0 || e.target.closest("button")) return;
      dragging = true;
      activate(win);
      var t = (win.style.translate || "0px 0px").split(" ");
      ox = parseFloat(t[0]) || 0; oy = parseFloat(t[1]) || 0;
      sx = e.clientX; sy = e.clientY;
      bar.setPointerCapture(e.pointerId);
      e.preventDefault();
    });
    bar.addEventListener("pointermove", function (e) {
      if (!dragging) return;
      win.style.translate = (ox + e.clientX - sx) + "px " + (oy + e.clientY - sy) + "px";
    });
    function stop() { dragging = false; }
    bar.addEventListener("pointerup", stop);
    bar.addEventListener("pointercancel", stop);
    // Double-click the title bar to put a dragged window back where it started.
    bar.addEventListener("dblclick", function () { win.style.translate = ""; });
  }

  var mainWins = $all("main > .win");
  mainWins.forEach(function (win) {
    win.addEventListener("pointerdown", function () { activate(win); });
    win.addEventListener("focusin", function () { activate(win); });
  });
  function setupDesktop() {
    if (!desktopMode.matches) return;
    mainWins.forEach(function (win) { makeControls(win); makeDraggable(win); });
  }
  setupDesktop();
  if (desktopMode.addEventListener) {
    desktopMode.addEventListener("change", function () {
      // Leaving desktop width: undo drags and bring back minimised windows so a
      // narrow layout never hides content.
      if (!desktopMode.matches) {
        mainWins.forEach(function (w) {
          w.style.translate = ""; w.style.zIndex = ""; w.classList.remove("is-inactive");
          if (w.hidden) restore(w);
        });
        $all(".titlebar__ctrl").forEach(function (c) { c.remove(); });
      } else {
        mainWins.forEach(function (win) { if (!win.classList.contains("is-draggable")) makeDraggable(win); makeControls(win); });
      }
    });
  }

  /* ---------- Dialogs ---------- */
  function openDialog(id) {
    var d = document.getElementById(id);
    if (d && typeof d.showModal === "function" && !d.open) { closeStart(); d.showModal(); }
    return d;
  }
  $all("[data-open]").forEach(function (b) {
    b.addEventListener("click", function () { openDialog(b.dataset.open); });
  });
  $all("dialog [data-close]").forEach(function (b) {
    b.addEventListener("click", function () { b.closest("dialog").close(); });
  });
  var binClear = $("#bin-clear");
  if (binClear) {
    binClear.addEventListener("click", function () {
      $(".bin-full").hidden = true;
      $(".bin-empty").hidden = false;
      binClear.disabled = true;
    });
  }

  // The joke screens close on any key or click, as the originals claimed to.
  ["dlg-bsod", "dlg-shutdown"].forEach(function (id) {
    var d = document.getElementById(id);
    if (!d) return;
    d.addEventListener("keydown", function (e) { if (e.key !== "Tab") { e.preventDefault(); d.close(); } });
    d.addEventListener("click", function () { d.close(); });
  });
  var shutdownBtn = $("#shutdown-btn");
  if (shutdownBtn) shutdownBtn.addEventListener("click", function () { openDialog("dlg-shutdown"); });

  /* ---------- Konami code: a fake BSOD, clearly a joke ---------- */
  var KONAMI = ["ArrowUp", "ArrowUp", "ArrowDown", "ArrowDown", "ArrowLeft", "ArrowRight", "ArrowLeft", "ArrowRight", "b", "a"];
  var pos = 0;
  document.addEventListener("keydown", function (e) {
    if (document.querySelector("dialog[open]")) return;
    var key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
    pos = key === KONAMI[pos] ? pos + 1 : (key === KONAMI[0] ? 1 : 0);
    if (pos === KONAMI.length) { pos = 0; openDialog("dlg-bsod"); }
  });

  /* ---------- filianIsLost terminal ----------
     A fan nod to the @filianIsLost mood. Vague on purpose: Filian's own lines are
     only silence, so nothing is put in her mouth, and nothing claims to be lore. */
  var LOG = {
    ru: [
      { who: "???", text: "кто-нибудь ещё не спит?" },
      { who: "Snacker", text: "я здесь. и не только я." },
      { who: "???", text: "на канале тихо. слишком тихо." },
      { who: "Snacker", text: "значит, ждём. мы умеем ждать." },
      { who: "Filian", text: "...", slow: true },
      { silence: 1300 },
      { who: "???", glitch: "она п▒т▒р█на", text: "она где-то рядом." },
      { who: "Snacker", text: "тогда оставим свет включённым." }
    ],
    en: [
      { who: "???", text: "anyone else still awake?" },
      { who: "Snacker", text: "i'm here. not the only one." },
      { who: "???", text: "the channel is quiet. too quiet." },
      { who: "Snacker", text: "then we wait. we're good at waiting." },
      { who: "Filian", text: "...", slow: true },
      { silence: 1300 },
      { who: "???", glitch: "she is l▒st", text: "she is somewhere close." },
      { who: "Snacker", text: "then we leave the light on." }
    ]
  };
  var WHO_CLASS = { "Filian": "filian", "???": "someone", "Snacker": "snacker" };

  // Types the log into `screen` line by line. Returns a cancel function. With
  // `instant` it renders the finished log at once, for reduced motion.
  function typeLog(screen, lines, instant, onDone) {
    var timers = [], cancelled = false;
    function later(fn, ms) { timers.push(setTimeout(function () { if (!cancelled) fn(); }, ms)); }
    screen.textContent = "";
    var cursor = document.createElement("span");
    cursor.className = "term__cursor";
    cursor.setAttribute("aria-hidden", "true");

    function makeLine(item) {
      var row = document.createElement("div");
      row.className = "term__line";
      if (item.who) {
        var who = document.createElement("span");
        who.className = "term__who term__who--" + WHO_CLASS[item.who];
        who.textContent = item.who + ">";
        row.appendChild(who);
      }
      var text = document.createElement("span");
      text.className = "term__text";
      row.appendChild(text);
      screen.appendChild(row);
      return { row: row, text: text };
    }

    if (instant) {
      lines.forEach(function (item) { if (!item.silence) makeLine(item).text.textContent = item.text; });
      if (onDone) onDone();
      return function () {};
    }

    var i = 0;
    function type(target, str, speed, done) {
      var n = 0;
      (function step() {
        target.textContent = str.slice(0, n);
        target.appendChild(cursor);
        if (n++ < str.length) later(step, speed); else done();
      })();
    }
    function erase(target, done) {
      (function step() {
        var t = target.textContent;
        if (!t.length) { done(); return; }
        target.textContent = t.slice(0, -1);
        target.appendChild(cursor);
        later(step, 22);
      })();
    }
    function next() {
      if (i >= lines.length) { later(function () { cursor.remove(); if (onDone) onDone(); }, 1600); return; }
      var item = lines[i++];
      if (item.silence) {
        // The moment of silence: an empty line with only the cursor blinking.
        var quiet = makeLine({});
        quiet.text.appendChild(cursor);
        later(function () { quiet.row.remove(); next(); }, item.silence);
        return;
      }
      var line = makeLine(item);
      if (item.glitch) {
        type(line.text, item.glitch, 30, function () {
          line.row.classList.add("is-glitch");
          later(function () {
            line.row.classList.remove("is-glitch");
            erase(line.text, function () { type(line.text, item.text, 34, function () { later(next, 520); }); });
          }, 650);
        });
      } else {
        type(line.text, item.text, item.slow ? 460 : 34, function () { later(next, item.slow ? 700 : 480); });
      }
    }
    next();
    return function () { cancelled = true; timers.forEach(clearTimeout); };
  }

  function logLines() { return LOG[isEn() ? "en" : "ru"]; }
  function logText() {
    return logLines().filter(function (l) { return !l.silence; })
      .map(function (l) { return l.who + "> " + l.text; }).join(" ");
  }

  /* ---------- Replay: filianIsLost.log ---------- */
  var logDlg = $("#dlg-log");
  var logScreen = $("#log-screen");
  var stopLog = null;
  function playLogDialog() {
    if (stopLog) stopLog();
    // Screen readers get the whole log at once instead of a character stream.
    logScreen.setAttribute("aria-hidden", "true");
    $("#log-sr").textContent = logText();
    stopLog = typeLog(logScreen, logLines(), reduceMotion.matches, null);
  }
  if (logDlg) {
    $all("[data-open-log]").forEach(function (b) {
      b.addEventListener("click", function () {
        closeStart();
        if (!logDlg.open) logDlg.showModal();
        playLogDialog();
      });
    });
    $("#log-replay").addEventListener("click", playLogDialog);
    logDlg.addEventListener("close", function () { if (stopLog) stopLog(); stopLog = null; });
  }

  /* ---------- Boot screen ----------
     Long enough to read the log, skippable with Enter (or Escape) or three taps,
     once per session, never under reduced motion, and the visitor can switch it
     off from the welcome window. */
  var bootPref = $("#boot-pref");
  var bootOn = load("boot") !== "off";
  if (bootPref) {
    bootPref.checked = bootOn;
    bootPref.addEventListener("change", function () { save("boot", bootPref.checked ? "on" : "off"); });
  }
  var seen = false;
  try { seen = sessionStorage.getItem("filianru.booted") === "1"; sessionStorage.setItem("filianru.booted", "1"); } catch (e) {}
  if (bootOn && !seen && !reduceMotion.matches && !location.hash) boot();
  else root.classList.remove("booting");

  function boot() {
    var en = isEn();
    var before = en
      ? ["Snackers 95", "Loading FRUIT.VXD ... OK", "Opening filianIsLost.log"]
      : ["Snackers 95", "Загрузка FRUIT.VXD ... OK", "Открываю filianIsLost.log"];
    var after = en
      ? ["Looking for Russian-speaking Snackers ... found", "Starting desktop"]
      : ["Поиск русскоязычных Snackers ... найдены", "Запуск рабочего стола"];

    var el = document.createElement("div");
    el.className = "boot";
    el.setAttribute("role", "dialog");
    el.setAttribute("aria-label", en ? "Startup screen. Press Enter to skip." : "Заставка. Нажмите Enter, чтобы пропустить.");
    var skip = document.createElement("p");
    skip.className = "boot__skip";
    skip.textContent = en ? "Enter or three taps to skip" : "Enter или три нажатия, чтобы пропустить";
    el.appendChild(skip);
    document.body.appendChild(el);

    var timers = [], finished = false, stopTerm = null, taps = 0, hint = null, hintTimer = null;
    function later(fn, ms) { timers.push(setTimeout(function () { if (!finished) fn(); }, ms)); }
    function line(text) {
      var p = document.createElement("p");
      p.textContent = text;
      el.insertBefore(p, skip);
    }
    function finish() {
      if (finished) return;
      finished = true;
      timers.forEach(clearTimeout);
      clearTimeout(hintTimer);
      if (stopTerm) stopTerm();
      document.removeEventListener("keydown", onKey, true);
      root.classList.remove("booting");
      el.classList.add("is-leaving");
      setTimeout(function () { el.remove(); }, 200);
    }
    function onKey(e) {
      if (e.key === "Enter" || e.key === "Escape") { e.preventDefault(); finish(); }
    }
    function onTap() {
      taps += 1;
      if (taps >= 3) { finish(); return; }
      var left = 3 - taps;
      if (!hint) { hint = document.createElement("p"); hint.className = "boot__hint"; hint.setAttribute("role", "status"); el.appendChild(hint); }
      hint.textContent = en
        ? (left === 2 ? "Two more taps to skip" : "One more tap to skip")
        : (left === 2 ? "Ещё два нажатия, чтобы пропустить" : "Ещё одно нажатие, чтобы пропустить");
      hint.hidden = false;
      clearTimeout(hintTimer);
      hintTimer = setTimeout(function () { if (hint) hint.hidden = true; }, 1800);
    }

    function runTerm() {
      var win = document.createElement("div");
      win.className = "term-win";
      var bar = document.createElement("div");
      bar.className = "titlebar";
      bar.innerHTML = '<img src="icons/log.svg" alt="" width="16" height="16"><span class="titlebar__text">filianIsLost.log</span>';
      var screen = document.createElement("div");
      screen.className = "term";
      screen.setAttribute("aria-hidden", "true");
      var sr = document.createElement("p");
      sr.className = "sr-only";
      sr.textContent = logText();
      win.appendChild(bar);
      win.appendChild(sr);
      win.appendChild(screen);
      var note = document.createElement("p");
      note.className = "term__note";
      note.textContent = isEn()
        ? "A fan nod. Not part of any ARG, and not Filian's words."
        : "Фанатская отсылка. Это не часть ARG и не слова Filian.";
      win.appendChild(note);
      el.appendChild(win);
      stopTerm = typeLog(screen, logLines(), false, function () {
        win.classList.add("is-leaving");
        later(function () { win.remove(); runAfter(0); }, 260);
      });
    }
    function runBefore(i) {
      if (i < before.length) { line(before[i]); later(function () { runBefore(i + 1); }, 220); }
      else later(runTerm, 250);
    }
    function runAfter(i) {
      if (i < after.length) { line(after[i]); later(function () { runAfter(i + 1); }, 220); }
      else later(finish, 300);
    }

    el.addEventListener("click", onTap);
    document.addEventListener("keydown", onKey, true);
    // Failsafe: whatever happens to the timers, the boot never outlives this.
    setTimeout(finish, 30000);
    runBefore(0);
  }
})();
