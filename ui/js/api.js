// js/api.js:
window.App = window.App || {};

window.App.api = (() => {
  // Requests to a local daemon should be fast. A hung request (e.g. a
  // half-closed connection after the daemon restarts) must not leave the UI
  // waiting forever, so abort it and let the caller retry.
  const REQUEST_TIMEOUT_MS = 15000;

  async function fetchWithTimeout(url, opts = {}) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
    try {
      return await fetch(url, { ...opts, signal: controller.signal });
    } finally {
      clearTimeout(timer);
    }
  }

  async function getJSON(url) {
    const res = await fetchWithTimeout(url);
    if (!res.ok) {
      let detail = '';
      try {
        const data = await res.json();
        if (data && data.error) detail = `: ${data.error}`;
      } catch (_) {
        /* non-JSON error body */
      }
      throw new Error(`GET ${url} failed: ${res.status}${detail}`);
    }
    return res.json();
  }

  async function getState() {
    return getJSON('/api/state');
  }

  async function getView(view, params) {
    const qs = new URLSearchParams(params).toString();
    const res = await fetchWithTimeout(`/api/views/${view}${qs ? '?' + qs : ''}`);
    if (!res.ok) throw new Error(`GET /api/views/${view} failed: ${res.status}`);
    return res.text();
  }

  async function request(method, url, body) {
    const opts = { method, headers: {} };
    if (body !== undefined) {
      opts.headers['Content-Type'] = 'application/json';
      opts.body = JSON.stringify(body);
    }
    const res = await fetchWithTimeout(url, opts);
    if (!res.ok) throw new Error(`${method} ${url} failed: ${res.status}`);
    return res.json();
  }

  async function postJSON(url, body) {
    return request('POST', url, body);
  }

  async function deleteJSON(url) {
    return request('DELETE', url);
  }

  return { getJSON, getState, getView, postJSON, deleteJSON };
})();