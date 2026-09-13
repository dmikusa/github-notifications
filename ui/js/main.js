// js/main.js:
window.App = window.App || {};

window.App.status = (() => {
  function set(text, isError) {
    const el = document.getElementById('status-text');
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

// Transient toast for action feedback ("Marked 3 read", "Dismissed 2", ...).
window.App.flash = (() => {
  let timer = null;
  function show(message, isError) {
    const el = document.getElementById('flash');
    if (!el) return;
    el.textContent = message || '';
    el.classList.toggle('error', !!isError);
    el.hidden = false;
    clearTimeout(timer);
    timer = setTimeout(() => {
      el.hidden = true;
    }, 2600);
  }
  return { show };
})();

window.App.views = (() => {
  function params() {
    return { ws: window.App.state.currentWorkspace };
  }

  async function load(view) {
    // Remember the outgoing view's filters before the DOM is replaced.
    window.App.filters.capture(window.App.state.currentView);
    window.App.state.currentView = view;
    document.querySelectorAll('#tabs .tab').forEach((t) => {
      t.classList.toggle('active', t.dataset.view === view);
    });
    const html = await window.App.api.getView(view, {
      ...params(),
      ...window.App.filters.forView(view),
    });
    const el = document.getElementById('view');
    el.outerHTML = html;
    // The view was swapped outside of an htmx transaction, so htmx never
    // processed the new elements (filter controls, etc.). Register them now.
    const newEl = document.getElementById('view');
    if (newEl && window.htmx) htmx.process(newEl);
    window.App.filters.capture(view);
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
  setSyncRunning(!!status.running);
  if (status.last_sync) renderSyncText(status.last_sync);
  window.App.notice.update(status);
  // Load the initial view once the first sync has populated the cache. The
  // continuous poller (pollTimer) keeps running so background syncs keep the
  // spinner and "last sync" timestamp fresh.
  if (!viewLoaded && status.populated) {
    viewLoaded = true;
    await window.App.views.load(window.App.state.currentView);
  }
}

async function syncStatusTick() {
  try {
    const st = await window.App.api.getJSON('/api/sync/status');
    await onSyncStatus(st);
  } catch (_) {
    /* daemon unreachable momentarily; try again next tick */
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

let lastAuthText = '';

function renderStatusLine(state) {
  const auth = state.auth;
  let text = `auth: ${auth.provider}`;
  if (auth.ok) {
    text += ` as ${auth.login || 'unknown'}`;
  } else {
    text += ` (${auth.missing.join(', ') || 'unconfigured'})`;
  }
  lastAuthText = text;
  renderSyncText(state.sync.last_sync);
  window.App.notice.update(state.sync);
}

function renderSyncText(lastSync) {
  const text =
    lastAuthText + (lastSync ? ` \u00b7 last sync ${formatLocalTime(lastSync)}` : '');
  window.App.status.set(text);
}

/// Swap the refresh button for a spinner while a sync pass is running, so the
/// sync can't be triggered again mid-pass.
function setSyncRunning(running) {
  const btn = document.getElementById('sync');
  const spinner = document.getElementById('sync-spinner');
  if (!btn || !spinner) return;
  btn.hidden = running;
  spinner.hidden = !running;
}

/// Format an RFC3339 timestamp (from the API) in the user's local timezone,
/// e.g. `2026-09-06 13:30:41 EST`.
function formatLocalTime(iso) {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const pad = (n) => String(n).padStart(2, '0');
  const y = date.getFullYear();
  const mo = pad(date.getMonth() + 1);
  const d = pad(date.getDate());
  const h = pad(date.getHours());
  const mi = pad(date.getMinutes());
  const s = pad(date.getSeconds());
  const tz =
    new Intl.DateTimeFormat('en-US', { timeZoneName: 'short' })
      .formatToParts(date)
      .find((p) => p.type === 'timeZoneName')?.value ?? '';
  return `${y}-${mo}-${d} ${h}:${mi}:${s} ${tz}`;
}

async function refreshStatusLine() {
  try {
    const state = await window.App.api.getState();
    renderStatusLine(state);
    return state;
  } catch (err) {
    window.App.status.set(`Failed to reach the daemon: ${err.message}`, true);
    return null;
  }
}

(async () => {
  try {
    const state = await window.App.api.getState();
    populateWorkspaces(state);

    const wsSel = document.getElementById('ws');
    renderStatusLine(state);

    document.getElementById('tabs').addEventListener('click', (e) => {
      const tab = e.target.closest('.tab');
      if (tab) window.App.views.load(tab.dataset.view);
    });
    wsSel.addEventListener('change', async () => {
      window.App.state.currentWorkspace = wsSel.value;
      // Tell the server which workspace is active; it syncs that workspace's
      // repos and triggers a refresh if they're stale.
      await window.App.api.postJSON(
        `/api/workspaces/${encodeURIComponent(wsSel.value)}/activate`,
        {}
      );
      window.App.views.reload();
    });
    document.getElementById('sync').addEventListener('click', async () => {
      const btn = document.getElementById('sync');
      if (!btn || btn.hidden) return;
      setSyncRunning(true);
      window.App.flash.show('Syncing\u2026');
      try {
        // A manual sync runs in the background; poll /api/sync/status until
        // `last_sync` advances past the value at click time (or the pass fails).
        const before = (await window.App.api.getJSON('/api/sync/status')).last_sync;
        await window.App.api.postJSON('/api/sync', {});
        let outcome = { done: false, error: null };
        for (let i = 0; i < 180 && !outcome.done; i++) {
          await new Promise((r) => setTimeout(r, 1000));
          const st = await window.App.api.getJSON('/api/sync/status');
          if (st.last_sync && st.last_sync !== before) {
            outcome.done = true;
          } else if (!st.running && st.last_error) {
            outcome.done = true;
            outcome.error = st.last_error;
          }
        }
        // Refresh the data table and the "last sync" line once it's done.
        await window.App.views.reload();
        const state = await refreshStatusLine();
        if (outcome.error) {
          window.App.flash.show(`Sync failed: ${outcome.error}`, true);
        } else if (!outcome.done) {
          window.App.flash.show('Sync still running \u2014 check back shortly.', true);
        } else if (state?.sync?.last_sync) {
          window.App.flash.show(`Sync complete \u00b7 last sync ${formatLocalTime(state.sync.last_sync)}`);
        } else {
          window.App.flash.show('Sync complete');
        }
      } catch (err) {
        window.App.flash.show(`Sync failed: ${err.message}`, true);
      } finally {
        setSyncRunning(false);
      }
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
        await window.App.api.postJSON(
          `/api/workspaces/${encodeURIComponent(name)}/activate`,
          {}
        );
        populateWorkspaces(await window.App.api.getState());
        await window.App.views.reload();
      } catch (err) {
        window.App.status.set(`Add workspace failed: ${err.message}`, true);
      }
      dialog.close();
      document.getElementById('new-ws-name').value = '';
    });

    // Manual "dismiss closed/merged" from the inbox view. Runs in the
    // background; poll /api/sync/status until it reports completion. The
    // button swaps its icon for a spinner while the pass runs.
    document.addEventListener('click', async (e) => {
      const btn = e.target.closest('#dismiss-closed-merged');
      if (!btn || btn.classList.contains('working')) return;
      const originalHtml = btn.innerHTML;
      btn.classList.add('working');
      btn.innerHTML = '<span class="spinner" aria-hidden="true"></span>Dismiss all closed/merged';
      window.App.flash.show('Dismissing closed/merged notifications\u2026');
      try {
        await window.App.api.postJSON('/api/notifications/dismiss-closed-merged', {});
        let count = null;
        for (let i = 0; i < 180; i++) {
          await new Promise((r) => setTimeout(r, 1000));
          const st = await window.App.api.getJSON('/api/sync/status');
          if (!st.dismiss_running) {
            count = st.last_dismiss;
            break;
          }
        }
        await window.App.views.reload();
        if (count === null) {
          window.App.flash.show('Dismiss still running \u2014 check back shortly.', true);
        } else {
          window.App.flash.show(`Dismissed ${count || 0} closed/merged notification(s)`);
        }
      } catch (err) {
        window.App.flash.show(`Dismiss failed: ${err.message}`, true);
      } finally {
        btn.classList.remove('working');
        btn.innerHTML = originalHtml;
      }
    });

    const status = await window.App.api.getJSON('/api/sync/status');
    if (!status.populated) {
      showLoading();
    }
    // Continuous sync-status awareness: drives the spinner next to "last sync"
    // whenever a pass runs, keeps the timestamp fresh, and loads the initial
    // view once the first sync completes.
    await syncStatusTick();
    pollTimer = setInterval(syncStatusTick, 2000);
  } catch (err) {
    window.App.status.set(`Failed to reach the daemon: ${err.message}`, true);
  }
})();

document.addEventListener('htmx:afterSwap', () => {
  window.App.table.bind();
  window.App.filters.capture(window.App.state.currentView);
});