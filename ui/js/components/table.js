// js/components/table.js:
// Owns checkbox selection, the bulk-action bar, and bulk actions for the
// active view. The table markup (rows, filters, bulk bar) is server-rendered;
// this module only wires selection state and dispatches actions.
window.App = window.App || {};

window.App.table = (() => {
  function selected() {
    return Array.from(document.querySelectorAll('#view .sel:checked')).map((cb) => cb.value);
  }

  function selectedUrls() {
    return Array.from(document.querySelectorAll('#view .sel:checked'))
      .map((cb) => cb.dataset.htmlUrl)
      .filter(Boolean);
  }

  function updateBulkBar() {
    const root = document.querySelector('#view');
    if (!root) return;
    const bar = root.querySelector('.bulk-bar');
    const count = root.querySelector('.sel-count');
    const sel = selected();
    if (bar) bar.hidden = sel.length === 0;
    if (count) count.textContent = sel.length ? `${sel.length} selected` : '';
  }

  function bind() {
    const root = document.querySelector('#view');
    if (!root) return;

    const selAll = root.querySelector('.sel-all');
    if (selAll) {
      selAll.addEventListener('change', () => {
        root.querySelectorAll('.sel').forEach((cb) => {
          cb.checked = selAll.checked;
        });
        updateBulkBar();
      });
    }
    root.querySelectorAll('.sel').forEach((cb) => {
      cb.addEventListener('change', updateBulkBar);
    });

    const bar = root.querySelector('.bulk-bar');
    if (bar) {
      bar.querySelectorAll('button[data-action]').forEach((btn) => {
        btn.addEventListener('click', () => runAction(btn.dataset.action));
      });
    }
  }

  async function runAction(action) {
    const view = document.querySelector('#view')?.dataset.view;
    const ws = window.App.state.currentWorkspace;
    try {
      if (action === 'mark-read') {
        if (view === 'queue') {
          await window.App.api.postJSON('/api/issues/mark-read', {
            ids: selected().map(Number),
          });
        } else if (view === 'inbox') {
          await window.App.api.postJSON('/api/threads/mark-read', { ids: selected() });
        }
      } else if (action === 'mark-all-read') {
        await window.App.api.postJSON('/api/threads/mark-read', { all: true, ws });
      } else if (action === 'open') {
        selectedUrls().forEach((url) => window.open(url, '_blank', 'noopener'));
      } else if (action === 'watch' || action === 'unwatch' || action === 'ignore' || action === 'unignore') {
        for (const repo of selected()) {
          const [owner, name] = repo.split('/');
          await window.App.api.postJSON(`/api/repos/${encodeURIComponent(owner)}/${encodeURIComponent(name)}/${action}`, {});
        }
      } else {
        return;
      }
    } catch (err) {
      window.App.status.set(`Action failed: ${err.message}`, true);
      return;
    }
    window.App.views.reload();
  }

  return { bind, selected };
})();