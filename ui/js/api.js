// js/api.js:
window.App = window.App || {};

window.App.api = (() => {
  async function getState() {
    const res = await fetch('/api/state');
    if (!res.ok) {
      throw new Error(`GET /api/state failed: ${res.status}`);
    }
    return res.json();
  }

  return { getState };
})();