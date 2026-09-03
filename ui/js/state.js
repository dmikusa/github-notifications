// js/state.js:
window.App = window.App || {};

window.App.state = (() => {
  let current = null;

  async function refresh() {
    current = await window.App.api.getState();
    return current;
  }

  function get() {
    return current;
  }

  return { refresh, get };
})();