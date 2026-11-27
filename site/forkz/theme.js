// Sets the theme before the page paints (light, dark, high contrast, or
// the system's), and keeps the header's theme menu in step with it.
(function () {
  var saved = null;
  try {
    saved = localStorage.getItem("forkz-theme");
  } catch (e) {}
  if (saved === "light" || saved === "dark" || saved === "high-contrast") {
    document.documentElement.setAttribute("data-theme", saved);
  }
  document.addEventListener("DOMContentLoaded", function () {
    var select = document.getElementById("theme");
    if (!select) return;
    select.value = saved || "system";
    select.addEventListener("change", function () {
      var value = select.value;
      try {
        if (value === "system") localStorage.removeItem("forkz-theme");
        else localStorage.setItem("forkz-theme", value);
      } catch (e) {}
      if (value === "system") document.documentElement.removeAttribute("data-theme");
      else document.documentElement.setAttribute("data-theme", value);
    });
  });
})();
