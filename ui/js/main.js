// js/main.js:
window.App = window.App || {};

let knownLastSync = null;

window.App.status = (() => {
  function set(text, isError) {
    const el = document.getElementById('status');
    if (!el) return;
    el.textContent = text || '';
    el.classList.toggle('error', !!isError);
  }
  return { set };
})();

window.App.notice = (() => {
  function update(state) {
    const el = document.getElementById('notice');
    if (!el) return;
    const sync = state.sync;
    if (sync.last_sync) {
      el.hidden = true;
      el.textContent = '';
      return;
    }
    el.hidden = false;
    el.classList.remove('error');
    if (sync.running) {
      el.textContent =
        'Initial sync in progress \u2014 populating the cache from GitHub. ' +
        'This can take a minute on first run.';
    } else if (sync.last_error) {
      el.classList.add('error');
      el.textContent = `Initial sync failed: ${sync.last_error}. Check your auth and press Sync.`;
    } else {
      el.textContent = 'Cache not populated yet \u2014 waiting for the initial sync.';
    }
  }
  return { update };
})();

window.App.views = (() => {
  function params() {
    return { ws: window.App.state.currentWorkspace };
  }

  async function load(view) {
    window.App.state.currentView = view;
    document.querySelectorAll('#tabs .tab').forEach((t) => {
      t.classList.toggle('active', t.dataset.view === view);
    });
    const html = await window.App.api.getView(view, params());
    const el = document.getElementById('view');
    el.outerHTML = html;
    window.App.table.bind();
  }

  async function reload() {
    await load(window.App.state.currentView);
  }

  return { load, reload };
})();

(async () => {
  try {
    const state = await window.App.api.getState();
    knownLastSync = state.sync.last_sync;
    window.App.state.setWorkspaces(state.workspaces.map((w) => w.name));

    const wsSel = document.getElementById('ws');
    state.workspaces.forEach((w) => {
      const opt = document.createElement('option');
      opt.value = w.name;
      opt.textContent = w.name;
      wsSel.appendChild(opt);
    });
    if (window.App.state.currentWorkspace) {
      wsSel.value = window.App.state.currentWorkspace;
    }

    const auth = state.auth;
    let authText = `auth: ${auth.provider}`;
    if (auth.ok) {
      authText += ` as ${auth.login || 'unknown'}`;
    } else {
      authText += ` (${auth.missing.join(', ') || 'unconfigured'})`;
    }
    if (state.sync.last_sync) authText += ` \u00b7 last sync ${state.sync.last_sync}`;
    window.App.status.set(authText);
    window.App.notice.update(state);

    document.getElementById('tabs').addEventListener('click', (e) => {
      const tab = e.target.closest('.tab');
      if (tab) window.App.views.load(tab.dataset.view);
    });
    wsSel.addEventListener('change', () => {
      window.App.state.currentWorkspace = wsSel.value;
      window.App.views.reload();
    });
    document.getElementById('sync').addEventListener('click', async () => {
      await window.App.api.postJSON('/api/sync', {});
      window.App.status.set('sync requested');
    });

    await window.App.views.load('queue');

    // Poll for sync completion; reload once the first sync lands so the
    // initial empty view is replaced without a manual refresh.
    setInterval(async () => {
      try {
        const s = await window.App.api.getState();
        window.App.notice.update(s);
        if (knownLastSync === null && s.sync.last_sync) {
          knownLastSync = s.sync.last_sync;
          await window.App.views.reload();
        }
      } catch (_) {
        /* daemon unreachable momentarily; try again next tick */
      }
    }, 4000);
  } catch (err) {
    window.App.status.set(`Failed to reach the daemon: ${err.message}`, true);
  }
})();

document.addEventListener('htmx:afterSwap', () => {
  window.App.table.bind();
});