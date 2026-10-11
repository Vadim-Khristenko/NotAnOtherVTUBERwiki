// Tabs (`:::tabs` in Markdown). The server sends every tab one after another
// under its name, which reads fine with scripts off; this turns them into a
// row of tabs with the first one open. Arrow keys, Home and End move between
// tabs, as the ARIA tabs pattern asks. The open tab is kept in the address
// (#tab-<name>), so a link can open a page on a tab.
(function () {
  "use strict";
  var groups = document.querySelectorAll(".content .tabs");
  if (!groups.length) return;
  var slug = function (s) {
    return s.toLowerCase().trim().replace(/[^\p{L}\p{N}]+/gu, "-").replace(/^-|-$/g, "");
  };
  Array.prototype.forEach.call(groups, function (group, g) {
    var panels = Array.prototype.filter.call(group.children, function (el) {
      return el.classList.contains("tab-panel");
    });
    if (panels.length < 2) return;
    var list = document.createElement("div");
    list.className = "tab-list";
    list.setAttribute("role", "tablist");
    var tabs = panels.map(function (panel, i) {
      var title = panel.querySelector(".tab-title");
      var name = title ? title.textContent : String(i + 1);
      var tab = document.createElement("button");
      tab.type = "button";
      tab.className = "tab";
      tab.textContent = name;
      tab.id = "tab-" + g + "-" + i;
      tab.setAttribute("role", "tab");
      tab.setAttribute("aria-controls", "tabpanel-" + g + "-" + i);
      tab.dataset.key = slug(name);
      panel.id = "tabpanel-" + g + "-" + i;
      panel.setAttribute("role", "tabpanel");
      panel.setAttribute("aria-labelledby", tab.id);
      panel.tabIndex = 0;
      if (title) title.hidden = true;
      list.appendChild(tab);
      return tab;
    });
    var select = function (n, focus, remember) {
      tabs.forEach(function (tab, i) {
        var on = i === n;
        tab.setAttribute("aria-selected", on ? "true" : "false");
        tab.tabIndex = on ? 0 : -1;
        panels[i].hidden = !on;
      });
      if (focus) tabs[n].focus();
      if (remember && window.history && history.replaceState) {
        history.replaceState(null, "", "#tab-" + tabs[n].dataset.key);
      }
    };
    list.addEventListener("click", function (ev) {
      var n = tabs.indexOf(ev.target.closest(".tab"));
      if (n >= 0) select(n, false, true);
    });
    list.addEventListener("keydown", function (ev) {
      var n = tabs.indexOf(document.activeElement);
      if (n < 0) return;
      var next = { ArrowRight: n + 1, ArrowLeft: n - 1, Home: 0, End: tabs.length - 1 }[ev.key];
      if (next === undefined) return;
      ev.preventDefault();
      select((next + tabs.length) % tabs.length, true, true);
    });
    group.insertBefore(list, group.firstChild);
    group.classList.add("tabs-ready");
    var wanted = decodeURIComponent(location.hash.replace(/^#tab-/, ""));
    var start = tabs.findIndex(function (tab) { return tab.dataset.key === wanted; });
    select(start >= 0 ? start : 0, false, false);
  });
})();
