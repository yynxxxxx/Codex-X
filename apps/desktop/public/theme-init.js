(function () {
  var theme = "light";
  try {
    theme = window.localStorage.getItem("codexx.theme") === "dark" ? "dark" : "light";
  } catch (_error) {
    // Keep the light default when storage is unavailable.
  }
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.colorScheme = theme;
})();
