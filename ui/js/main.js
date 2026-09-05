// js/main.js:
window.App = window.App || {};

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
    if (state.last_sync) {
      el.hidden = true;
      el.textContent = '';
      return;
    }
    el.hidden = false;
    el.classList.remove('error');
    if (state.rebuild) {
      el.textContent =
        'Cache was rebuilt after a schema change \u2014 populating data from GitHub. ' +
        'This can take a minute.';
    } else if (state.running) {
      el.textContent =
        'Initial sync in progress \u2014 populating the cache from GitHub. ' +
        'This can take a minute on first run.';
    } else if (state.last_error) {
      el.classList.add('error');
      el.textContent = `Initial sync failed: ${state.last_error}. Check your auth and press Sync.`;
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
    // The view was swapped outside of an htmx transaction, so htmx never
    // processed the new elements (filter controls, etc.). Register them now.
    const newEl = document.getElementById('view');
    if (newEl && window.htmx) htmx.process(newEl);
    window.App.table.bind();
    if (view === 'settings') {
      window.App.settings.bind();
    }
  }

  async function reload() {
    await load(window.App.state.currentView);
  }

  return { load, reload };
})();

// While the cache has not been populated, show a loading placeholder instead
// of fetching a (necessarily empty) view. Once /api/sync/status reports the
// initial sync is done, load the real view.
let viewLoaded = false;
let pollTimer = null;

function showLoading() {
  const el = document.getElementById('view');
  el.innerHTML =
    '<div class="loading"><span class="spinner" aria-hidden="true"></span>Loading data from GitHub\u2026</div>';
}

async function onSyncStatus(status) {
  window.App.notice.update(status);
  if (!viewLoaded && status.populated) {
    viewLoaded = true;
    clearInterval(pollTimer);
    pollTimer = null;
    await window.App.views.load(window.App.state.currentView);
  }
}

function populateWorkspaces(state) {
  window.App.state.setWorkspaces(state.workspaces.map((w) => w.name));
  const wsSel = document.getElementById('ws');
  wsSel.innerHTML = '';
  state.workspaces.forEach((w) => {
    const opt = document.createElement('option');
    opt.value = w.name;
    opt.textContent = w.name;
    wsSel.appendChild(opt);
  });
  if (window.App.state.currentWorkspace) {
    wsSel.value = window.App.state.currentWorkspace;
  }
}

(async () => {
  try {
    const state = await window.App.api.getState();
    populateWorkspaces(state);

    const wsSel = document.getElementById('ws');
    const auth = state.auth;
    let authText = `auth: ${auth.provider}`;
    if (auth.ok) {
      authText += ` as ${auth.login || 'unknown'}`;
    } else {
      authText += ` (${auth.missing.join(', ') || 'unconfigured'})`;
    }
    if (state.sync.last_sync) authText += ` \u00b7 last sync ${state.sync.last_sync}`;
    window.App.status.set(authText);
    window.App.notice.update(state.sync);

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

    const dialog = document.getElementById('add-ws-dialog');
    document.getElementById('add-ws').addEventListener('click', () => dialog.showModal());
    document.getElementById('add-ws-cancel').addEventListener('click', () => dialog.close());
    document.getElementById('add-ws-form').addEventListener('submit', async (e) => {
      e.preventDefault();
      const name = document.getElementById('new-ws-name').value.trim();
      if (!name) return;
      try {
        await window.App.api.postJSON('/api/workspaces', { name });
        window.App.state.currentWorkspace = name;
        populateWorkspaces(await window.App.api.getState());
        await window.App.views.reload();
      } catch (err) {
        window.App.status.set(`Add workspace failed: ${err.message}`, true);
      }
      dialog.close();
      document.getElementById('new-ws-name').value = '';
    });

    // Manual "dismiss closed/merged" from the inbox view. Runs in the
    // background; poll /api/sync/status until it reports completion.
    document.addEventListener('click', async (e) => {
      if (e.target.id !== 'dismiss-closed-merged') return;
      const btn = e.target;
      btn.disabled = true;
      const original = btn.textContent;
      btn.textContent = 'Dismissing\u2026';
      try {
        await window.App.api.postJSON('/api/notifications/dismiss-closed-merged', {});
        window.App.status.set('Dismissing closed/merged notifications\u2026');
        let count = null;
        for (let i = 0; i < 180; i++) {
          await new Promise((r) => setTimeout(r, 1000));
          const st = await window.App.api.getJSON('/api/sync/status');
          if (!st.dismiss_running) {
            count = st.last_dismiss;
            break;
          }
        }
        window.App.status.set(
          count === null ? 'Dismiss still running' : `Dismissed ${count || 0} closed/merged notification(s)`
        );
        await window.App.views.reload();
      } catch (err) {
        window.App.status.set(`Dismiss failed: ${err.message}`, true);
      }
      btn.disabled = false;
      btn.textContent = original;
    });

    const status = await window.App.api.getJSON('/api/sync/status');
    if (status.populated) {
      viewLoaded = true;
      await window.App.views.load('queue');
    } else {
      showLoading();
      pollTimer = setInterval(async () => {
        try {
          const s = await window.App.api.getJSON('/api/sync/status');
          await onSyncStatus(s);
        } catch (_) {
          /* daemon unreachable momentarily; try again next tick */
        }
      }, 4000);
    }
  } catch (err) {
    window.App.status.set(`Failed to reach the daemon: ${err.message}`, true);
  }
})();

document.addEventListener('htmx:afterSwap', () => {
  window.App.table.bind();
});