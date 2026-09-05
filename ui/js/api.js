// js/api.js:
window.App = window.App || {};

window.App.api = (() => {
  async function getState() {
    const res = await fetch('/api/state');
    if (!res.ok) throw new Error(`GET /api/state failed: ${res.status}`);
    return res.json();
  }

  async function getView(view, params) {
    const qs = new URLSearchParams(params).toString();
    const res = await fetch(`/api/views/${view}${qs ? '?' + qs : ''}`);
    if (!res.ok) throw new Error(`GET /api/views/${view} failed: ${res.status}`);
    return res.text();
  }

  async function postJSON(url, body) {
    const res = await fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    if (!res.ok) throw new Error(`POST ${url} failed: ${res.status}`);
    return res.json();
  }

  return { getState, getView, postJSON };
})();