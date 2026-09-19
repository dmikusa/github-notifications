// js/components/table.js:
// Owns checkbox selection, the bulk-action bar, and bulk actions for the
// active view. The table markup (rows, filters, bulk bar) is server-rendered;
// this module only wires selection state and dispatches actions.
window.App = window.App || {};

window.App.table = (() => {
  // The row checkbox a plain click last landed on. Shift-clicking another row
  // selects (or deselects) everything between the two. Kept fixed across
  // successive shift-clicks so a range can be expanded, and reset whenever the
  // rows are re-rendered (see bind).
  let anchor = null;

  function rows(root) {
    return Array.from((root || document.querySelector('#view'))?.querySelectorAll('.sel') || []);
  }

  function selected() {
    return Array.from(document.querySelectorAll('#view .sel:checked')).map((cb) => cb.value);
  }

  function selectedUrls() {
    return Array.from(document.querySelectorAll('#view .sel:checked'))
      .map((cb) => cb.dataset.htmlUrl)
      .filter(Boolean);
  }

  // Apply `to`'s checked state to every row between the anchor and `to`,
  // inclusive — the familiar shift-click range select.
  function applyRange(boxes, from, to) {
    const a = boxes.indexOf(from);
    const b = boxes.indexOf(to);
    if (a === -1 || b === -1) return;
    const [lo, hi] = a < b ? [a, b] : [b, a];
    for (let i = lo; i <= hi; i++) boxes[i].checked = to.checked;
  }

  function updateBulkBar() {
    const root = document.querySelector('#view');
    if (!root) return;
    const bar = root.querySelector('.bulk-bar');
    const count = root.querySelector('.sel-count');
    const sel = selected();
    // The action buttons stay visible at all times so the layout never shifts;
    // they're just disabled until something is selected.
    if (bar) {
      bar.querySelectorAll('button[data-action]').forEach((btn) => {
        btn.disabled = sel.length === 0;
      });
    }
    if (count) count.textContent = sel.length ? `${sel.length} selected` : '';
    // Reflect partial selection on the header checkbox (checked when all rows
    // are selected, indeterminate when some are).
    const selAll = root.querySelector('.sel-all');
    if (selAll) {
      const boxes = rows(root);
      const checked = boxes.filter((cb) => cb.checked).length;
      selAll.checked = boxes.length > 0 && checked === boxes.length;
      selAll.indeterminate = checked > 0 && checked < boxes.length;
    }
  }

  function bind() {
    const root = document.querySelector('#view');
    if (!root) return;

    // Fresh rows invalidate the old anchor element.
    anchor = null;

    const selAll = root.querySelector('.sel-all');
    if (selAll) {
      selAll.addEventListener('change', () => {
        root.querySelectorAll('.sel').forEach((cb) => {
          cb.checked = selAll.checked;
        });
        selAll.indeterminate = false;
        updateBulkBar();
      });
    }
    root.querySelectorAll('.sel').forEach((cb) => {
      cb.addEventListener('change', updateBulkBar);
      cb.addEventListener('click', (e) => {
        const boxes = rows(root);
        if (e.shiftKey && anchor && anchor !== cb && boxes.includes(anchor)) {
          applyRange(boxes, anchor, cb);
          updateBulkBar();
        } else {
          anchor = cb;
        }
      });
    });

    const bar = root.querySelector('.bulk-bar');
    if (bar) {
      bar.querySelectorAll('button[data-action]').forEach((btn) => {
        btn.addEventListener('click', () => runAction(btn.dataset.action));
      });
    }
    // Disable the action buttons until something is selected.
    updateBulkBar();
  }

  async function runAction(action) {
    const view = document.querySelector('#view')?.dataset.view;
    const ws = window.App.state.currentWorkspace;
    let result = null;
    try {
      if (action === 'mark-read') {
        if (view === 'queue') {
          result = await window.App.api.postJSON('/api/issues/mark-read', {
            ids: selected().map(Number),
          });
        } else if (view === 'inbox') {
          result = await window.App.api.postJSON('/api/threads/mark-read', { ids: selected() });
        }
      } else if (action === 'mark-all-read') {
        result = await window.App.api.postJSON('/api/threads/mark-read', { all: true, ws });
      } else if (action === 'mute') {
        result = await window.App.api.postJSON('/api/threads/mute', { ids: selected() });
      } else if (action === 'open') {
        selectedUrls().forEach((url) => window.open(url, '_blank', 'noopener'));
      } else if (action === 'watch' || action === 'unwatch' || action === 'ignore' || action === 'unignore') {
        for (const repo of selected()) {
          const [owner, name] = repo.split('/');
          await window.App.api.postJSON(`/api/repos/${encodeURIComponent(owner)}/${encodeURIComponent(name)}/${action}`, {});
        }
        result = selected().length;
      } else {
        return;
      }
    } catch (err) {
      window.App.flash.show(`Action failed: ${err.message}`, true);
      return;
    }
    const n = typeof result === 'number' ? result : selected().length;
    const labels = {
      'mark-read': 'Marked read',
      'mark-all-read': 'Marked all read',
      'mute': 'Dismissed',
      'open': 'Opened',
      'watch': 'Watched',
      'unwatch': 'Unwatched',
      'ignore': 'Ignored',
      'unignore': 'Unignored',
    };
    window.App.flash.show(`${labels[action]} ${n || ''}`.trim());
    window.App.views.reload();
  }

  return { bind, selected };
})();